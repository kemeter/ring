//! Firecracker microVM runtime — boot-minimal implementation.
//!
//! Scope of this first cut: boot `replicas` worker microVMs from a kernel
//! (config) + a per-deployment rootfs (`deployment.image` on the host), track
//! them by their API socket, scale up/down, and tear down. No networking, no
//! volumes, no stats, no health probes yet — those reuse the shared helpers
//! (`host_net`, `port_forwarder`, `virtiofs`, `vsock_client`) in later phases,
//! exactly as the Cloud Hypervisor runtime does.
//!
//! Mirrors `cloud_hypervisor::lifecycle` structure: a `*RuntimeConfig` with
//! defaults + `is_available` + `from_user_config`, a lifecycle struct holding
//! per-instance PIDs, and an instance id of `<deployment_id>-<tiny_id>` whose
//! presence on disk (its `.sock`) is the source of truth for "is it running".

use crate::config::server::FirecrackerConfig;
use crate::models::deployments::{Deployment, DeploymentStatus};
use crate::models::volume::ResolvedMount;
use crate::runtime::docker::tiny_id;
use crate::runtime::error::RuntimeError;
use crate::runtime::firecracker::client::{BootSource, Drive, FirecrackerClient, MachineConfig};
use crate::runtime::lifecycle_trait::RuntimeLifecycle;
use async_trait::async_trait;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use tracing::{error, info, warn};

/// Resolved Firecracker runtime config (defaults merged with user config).
#[derive(Debug, Clone)]
pub(crate) struct FirecrackerRuntimeConfig {
    /// Path to the `firecracker` binary.
    pub binary_path: String,
    /// Path to the uncompressed kernel image (`vmlinux`). Firecracker boots a
    /// kernel directly; there is no firmware step.
    pub kernel_path: String,
    /// Directory for per-VM API sockets and writable rootfs copies.
    pub socket_dir: String,
    /// Kernel command line. The default enables the serial console so console
    /// logs are capturable, and panics reboot rather than hang.
    pub boot_args: String,
}

impl Default for FirecrackerRuntimeConfig {
    fn default() -> Self {
        let base_dir = crate::config::config::get_config_dir();
        Self {
            binary_path: "firecracker".to_string(),
            kernel_path: format!("{}/firecracker/vmlinux", base_dir),
            socket_dir: format!("{}/firecracker/sockets", base_dir),
            boot_args: "console=ttyS0 reboot=k panic=1 pci=off".to_string(),
        }
    }
}

impl FirecrackerRuntimeConfig {
    /// Whether the `firecracker` binary is resolvable, so the runtime is only
    /// registered when it can actually run. Mirrors the CH `is_available` gate.
    pub(crate) fn is_available(&self) -> bool {
        let binary = &self.binary_path;
        if binary.contains('/') {
            return Path::new(binary).exists();
        }
        std::env::var_os("PATH")
            .map(|paths| std::env::split_paths(&paths).any(|dir| dir.join(binary).exists()))
            .unwrap_or(false)
    }

    /// Merge a user-facing config section with the defaults.
    pub(crate) fn from_user_config(user: &FirecrackerConfig) -> Self {
        let defaults = Self::default();
        Self {
            binary_path: user.binary_path.clone().unwrap_or(defaults.binary_path),
            kernel_path: user.kernel_path.clone().unwrap_or(defaults.kernel_path),
            socket_dir: user.socket_dir.clone().unwrap_or(defaults.socket_dir),
            boot_args: user.boot_args.clone().unwrap_or(defaults.boot_args),
        }
    }
}

pub struct FirecrackerLifecycle {
    config: FirecrackerRuntimeConfig,
    /// PID per instance id, captured at spawn so teardown can kill the right
    /// process. Absence means the VM is gone (or was never tracked by this
    /// process — e.g. inherited across a ring-server restart).
    pids: Mutex<HashMap<String, u32>>,
}

impl FirecrackerLifecycle {
    pub fn new(config: FirecrackerRuntimeConfig) -> Self {
        Self {
            config,
            pids: Mutex::new(HashMap::new()),
        }
    }

    fn socket_path(&self, instance_id: &str) -> String {
        format!("{}/{}.sock", self.config.socket_dir, instance_id)
    }

    fn rootfs_path(&self, instance_id: &str) -> String {
        format!("{}/{}.ext4", self.config.socket_dir, instance_id)
    }

    /// Boot one worker microVM. Returns the new instance id on success.
    async fn start_vm(&self, deployment: &Deployment) -> Result<String, RuntimeError> {
        // Pre-flight: kernel + base rootfs must exist before we spawn anything.
        if !Path::new(&self.config.kernel_path).exists() {
            return Err(RuntimeError::VmStartFailed(format!(
                "kernel image not found at '{}' (set [server.runtime.firecracker] kernel_path)",
                self.config.kernel_path
            )));
        }
        if !Path::new(&deployment.image).exists() {
            return Err(RuntimeError::ImageNotFound(format!(
                "rootfs image '{}' not found on host",
                deployment.image
            )));
        }

        std::fs::create_dir_all(&self.config.socket_dir).map_err(|e| {
            RuntimeError::VmStartFailed(format!(
                "could not create socket_dir '{}': {}",
                self.config.socket_dir, e
            ))
        })?;

        let instance_id = format!("{}-{}", deployment.id, tiny_id());
        let socket_path = self.socket_path(&instance_id);
        let rootfs_rw = self.rootfs_path(&instance_id);

        // Firecracker mutates the rootfs in place; give each VM a private copy
        // so replicas and reboots don't share guest state.
        std::fs::copy(&deployment.image, &rootfs_rw).map_err(|e| {
            RuntimeError::VmStartFailed(format!(
                "could not copy rootfs '{}' -> '{}': {}",
                deployment.image, rootfs_rw, e
            ))
        })?;

        // Spawn the firecracker process bound to its API socket.
        let child = std::process::Command::new(&self.config.binary_path)
            .arg("--api-sock")
            .arg(&socket_path)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| {
                RuntimeError::VmStartFailed(format!("could not spawn firecracker: {}", e))
            })?;
        let pid = child.id();

        // Wait for the API socket to appear (process creates it on startup).
        let mut ready = false;
        for _ in 0..50 {
            if Path::new(&socket_path).exists() {
                ready = true;
                break;
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        }
        if !ready {
            let _ = self.kill_pid(pid);
            let _ = std::fs::remove_file(&rootfs_rw);
            return Err(RuntimeError::VmStartFailed(
                "firecracker API socket never appeared".to_string(),
            ));
        }

        // Configure + boot via the REST API (the spike's PUT sequence).
        let client = FirecrackerClient::new(&socket_path);
        let boot = self
            .configure_and_boot(&client, &rootfs_rw, deployment)
            .await;
        if let Err(e) = boot {
            let _ = self.kill_pid(pid);
            let _ = std::fs::remove_file(&socket_path);
            let _ = std::fs::remove_file(&rootfs_rw);
            return Err(RuntimeError::VmStartFailed(format!(
                "configure/boot failed for {}: {}",
                instance_id, e
            )));
        }

        self.pids.lock().unwrap().insert(instance_id.clone(), pid);
        info!("Firecracker microVM {} booted (pid {})", instance_id, pid);
        Ok(instance_id)
    }

    async fn configure_and_boot(
        &self,
        client: &FirecrackerClient,
        rootfs_rw: &str,
        deployment: &Deployment,
    ) -> Result<(), String> {
        client
            .put_boot_source(&BootSource {
                kernel_image_path: self.config.kernel_path.clone(),
                boot_args: Some(self.config.boot_args.clone()),
                initrd_path: None,
            })
            .await
            .map_err(|e| e.to_string())?;

        client
            .put_drive(&Drive {
                drive_id: "rootfs".to_string(),
                path_on_host: rootfs_rw.to_string(),
                is_root_device: true,
                is_read_only: false,
            })
            .await
            .map_err(|e| e.to_string())?;

        let (vcpus, mem_mib) = parse_resources(deployment);
        client
            .put_machine_config(&MachineConfig {
                vcpu_count: vcpus,
                mem_size_mib: mem_mib,
            })
            .await
            .map_err(|e| e.to_string())?;

        client.start().await.map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Tear down one instance: graceful shutdown, kill the process, unlink the
    /// socket + rootfs copy. Returns true if the instance is gone afterwards.
    async fn stop_vm(&self, instance_id: &str) -> bool {
        let socket_path = self.socket_path(instance_id);

        // Best-effort graceful shutdown if the socket is still live.
        if Path::new(&socket_path).exists() {
            let client = FirecrackerClient::new(&socket_path);
            let _ = client.send_ctrl_alt_del().await;
            tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;
        }

        let pid = self.pids.lock().unwrap().remove(instance_id);
        if let Some(pid) = pid {
            let _ = self.kill_pid(pid);
        }
        let _ = std::fs::remove_file(&socket_path);
        let _ = std::fs::remove_file(self.rootfs_path(instance_id));
        !Path::new(&socket_path).exists()
    }

    fn kill_pid(&self, pid: u32) -> std::io::Result<()> {
        // SIGTERM the firecracker process; it exits when the guest is down or
        // when its VMM thread is signalled.
        std::process::Command::new("kill")
            .arg(pid.to_string())
            .status()
            .map(|_| ())
    }

    /// Instance ids currently tracked for a deployment whose socket still
    /// exists. The socket file is the on-disk source of truth for "running".
    fn scan_instances(&self, deployment_id: &str) -> Vec<String> {
        let prefix = format!("{}-", deployment_id);
        self.pids
            .lock()
            .unwrap()
            .keys()
            .filter(|id| id.starts_with(&prefix))
            .filter(|id| Path::new(&self.socket_path(id)).exists())
            .cloned()
            .collect()
    }

    async fn handle_worker_deployment(&self, mut deployment: Deployment) -> Deployment {
        let current = self.scan_instances(&deployment.id);
        let desired = deployment.replicas as usize;

        if current.len() < desired {
            for _ in current.len()..desired {
                match self.start_vm(&deployment).await {
                    Ok(_) => {}
                    Err(e) => {
                        error!("Firecracker: failed to start instance: {}", e);
                        deployment.status = DeploymentStatus::CreateContainerError;
                        break;
                    }
                }
            }
        } else if current.len() > desired {
            for instance_id in current.iter().skip(desired) {
                if !self.stop_vm(instance_id).await {
                    warn!("Firecracker: failed to stop instance {}", instance_id);
                }
            }
        }

        deployment.instances = self.scan_instances(&deployment.id);
        if deployment.status != DeploymentStatus::CreateContainerError {
            deployment.status = DeploymentStatus::Running;
        }
        deployment
    }
}

/// Parse vCPU count + memory (MiB) from the deployment's resource limits.
/// Defaults to 1 vCPU / 128 MiB — the same minimal microVM the spike booted.
fn parse_resources(_deployment: &Deployment) -> (u32, u32) {
    // TODO(firecracker): wire deployment.resources.limits.{cpu,memory} like CH
    // does via runtime::resources once the boot path is validated end-to-end.
    (1, 128)
}

#[async_trait]
impl RuntimeLifecycle for FirecrackerLifecycle {
    async fn apply(
        &self,
        mut deployment: Deployment,
        _resolved_mounts: Vec<ResolvedMount>,
    ) -> Deployment {
        if deployment.status == DeploymentStatus::Deleted {
            for instance_id in self.scan_instances(&deployment.id) {
                if !self.stop_vm(&instance_id).await {
                    warn!(
                        "Firecracker: failed to stop {} during deletion",
                        instance_id
                    );
                }
            }
            deployment.instances.clear();
            return deployment;
        }

        if deployment.kind == "job" {
            warn!("Firecracker: kind 'job' not yet supported, treating as worker");
        }
        self.handle_worker_deployment(deployment).await
    }

    async fn list_instances(&self, deployment_id: String, _status: &str) -> Vec<String> {
        self.scan_instances(&deployment_id)
    }

    async fn remove_instance(&self, instance_id: String) -> bool {
        self.stop_vm(&instance_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_uses_config_dir_paths() {
        let cfg = FirecrackerRuntimeConfig::default();
        assert_eq!(cfg.binary_path, "firecracker");
        assert!(cfg.kernel_path.ends_with("/firecracker/vmlinux"));
        assert!(cfg.socket_dir.ends_with("/firecracker/sockets"));
        assert!(cfg.boot_args.contains("console=ttyS0"));
    }

    #[test]
    fn from_user_config_overrides_only_set_fields() {
        let user = FirecrackerConfig {
            enabled: true,
            binary_path: Some("/opt/fc/firecracker".to_string()),
            kernel_path: None,
            socket_dir: Some("/var/run/fc".to_string()),
            boot_args: None,
        };
        let cfg = FirecrackerRuntimeConfig::from_user_config(&user);
        assert_eq!(cfg.binary_path, "/opt/fc/firecracker");
        assert_eq!(cfg.socket_dir, "/var/run/fc");
        // Unset fields fall back to defaults.
        let defaults = FirecrackerRuntimeConfig::default();
        assert_eq!(cfg.kernel_path, defaults.kernel_path);
        assert_eq!(cfg.boot_args, defaults.boot_args);
    }

    #[test]
    fn is_available_false_for_missing_absolute_path() {
        let cfg = FirecrackerRuntimeConfig {
            binary_path: "/nonexistent/firecracker".to_string(),
            ..FirecrackerRuntimeConfig::default()
        };
        assert!(!cfg.is_available());
    }

    #[test]
    fn socket_and_rootfs_paths_are_namespaced_by_instance() {
        let cfg = FirecrackerRuntimeConfig {
            socket_dir: "/tmp/fc".to_string(),
            ..FirecrackerRuntimeConfig::default()
        };
        let lc = FirecrackerLifecycle::new(cfg);
        assert_eq!(lc.socket_path("dep-1-abc"), "/tmp/fc/dep-1-abc.sock");
        assert_eq!(lc.rootfs_path("dep-1-abc"), "/tmp/fc/dep-1-abc.ext4");
    }
}
