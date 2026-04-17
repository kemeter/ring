use super::client::{
    CloudHypervisorClient, ConsoleConfig, CpuConfig, DiskConfig, MemoryConfig, NetConfig,
    PayloadConfig, VmConfig,
};
use crate::models::deployments::{Deployment, DeploymentStatus};
use crate::models::volume::ResolvedMount;
use crate::runtime::lifecycle_trait::RuntimeLifecycle;
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::Command;

/// Resolved runtime configuration for Cloud Hypervisor.
///
/// Built from the user-facing `config::config::CloudHypervisorConfig` with
/// defaults filled in. Paths here are always absolute and ready to use.
pub(crate) struct CloudHypervisorRuntimeConfig {
    /// Path to the cloud-hypervisor binary.
    pub binary_path: String,
    /// Path to the firmware (hypervisor-fw).
    pub firmware_path: String,
    /// Directory for VM API sockets and instance disk images.
    pub socket_dir: String,
}

impl Default for CloudHypervisorRuntimeConfig {
    fn default() -> Self {
        let base_dir = crate::config::config::get_config_dir();
        Self {
            binary_path: "cloud-hypervisor".to_string(),
            firmware_path: format!("{}/cloud-hypervisor/vmlinux", base_dir),
            socket_dir: format!("{}/cloud-hypervisor/sockets", base_dir),
        }
    }
}

impl CloudHypervisorRuntimeConfig {
    /// Merge a user-facing config section with the defaults. Any field left
    /// unset in `user` falls back to `CloudHypervisorRuntimeConfig::default`.
    pub(crate) fn from_user_config(
        user: &crate::config::config::CloudHypervisorConfig,
    ) -> Self {
        let defaults = Self::default();
        Self {
            binary_path: user
                .binary_path
                .clone()
                .unwrap_or(defaults.binary_path),
            firmware_path: user
                .firmware_path
                .clone()
                .unwrap_or(defaults.firmware_path),
            socket_dir: user.socket_dir.clone().unwrap_or(defaults.socket_dir),
        }
    }
}

pub struct CloudHypervisorLifecycle {
    config: CloudHypervisorRuntimeConfig,
}

impl CloudHypervisorLifecycle {
    pub fn new(config: CloudHypervisorRuntimeConfig) -> Self {
        Self { config }
    }

    fn socket_path(&self, instance_id: &str) -> PathBuf {
        PathBuf::from(&self.config.socket_dir).join(format!("{}.sock", instance_id))
    }

    fn instance_image_path(&self, instance_id: &str) -> PathBuf {
        PathBuf::from(&self.config.socket_dir).join(format!("{}.raw", instance_id))
    }

    fn deployment_prefix(deployment_id: &str) -> String {
        format!("ch-{}-", &deployment_id[..8.min(deployment_id.len())])
    }

    /// Scan sockets directory for instances belonging to a deployment.
    /// Returns instance IDs whose VMs are in one of the given states.
    /// An empty `accepted_states` slice matches any state (including unreachable VMs).
    async fn scan_instances(&self, deployment_id: &str, accepted_states: &[&str]) -> Vec<String> {
        let prefix = Self::deployment_prefix(deployment_id);
        let mut instances = Vec::new();

        let mut entries = match tokio::fs::read_dir(&self.config.socket_dir).await {
            Ok(e) => e,
            Err(_) => return instances,
        };

        while let Ok(Some(entry)) = entries.next_entry().await {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with(&prefix) || !name.ends_with(".sock") {
                continue;
            }

            let instance_id = name.trim_end_matches(".sock").to_string();
            let socket_str = match entry.path().to_str() {
                Some(s) => s.to_string(),
                None => continue,
            };

            if accepted_states.is_empty() {
                instances.push(instance_id);
                continue;
            }

            let client = CloudHypervisorClient::new(&socket_str);
            if let Ok(info) = client.info().await {
                if accepted_states.contains(&info.state.as_str()) {
                    instances.push(instance_id);
                }
            }
        }

        instances
    }

    fn path_str(path: &Path) -> Result<&str, String> {
        path.to_str()
            .ok_or_else(|| format!("Path contains non-UTF-8 characters: {:?}", path))
    }

    /// Start the cloud-hypervisor process for a VM instance.
    async fn start_vm_process(
        &self,
        instance_id: &str,
        deployment: &Deployment,
    ) -> Result<(), String> {
        let socket = self.socket_path(instance_id);
        let socket_str = Self::path_str(&socket)?;

        // Ensure socket directory exists
        tokio::fs::create_dir_all(&self.config.socket_dir)
            .await
            .map_err(|e| format!("Failed to create socket dir: {}", e))?;

        // Each instance gets a sparse copy of the base image
        let base_image = PathBuf::from(&deployment.image);
        if !base_image.exists() {
            return Err(format!("VM image not found: {}", deployment.image));
        }

        let instance_image = self.instance_image_path(instance_id);
        if !instance_image.exists() {
            let output = Command::new("cp")
                .args([
                    "--sparse=always",
                    Self::path_str(&base_image)?,
                    Self::path_str(&instance_image)?,
                ])
                .output()
                .await
                .map_err(|e| format!("Failed to copy VM image: {}", e))?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(format!("Failed to copy VM image: {}", stderr));
            }
        }

        // Parse resource limits
        let (vcpus, memory_mb) = parse_resources(deployment);

        // Start cloud-hypervisor process
        let mut child = Command::new(&self.config.binary_path)
            .arg("--api-socket")
            .arg(socket_str)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("Failed to start cloud-hypervisor: {}", e))?;

        // Monitor the child process in the background
        let child_instance_id = instance_id.to_string();
        tokio::spawn(async move {
            match child.wait().await {
                Ok(status) => {
                    warn!(
                        "cloud-hypervisor process for {} exited with {}",
                        child_instance_id, status
                    );
                }
                Err(e) => {
                    error!(
                        "Failed to wait for cloud-hypervisor process {}: {}",
                        child_instance_id, e
                    );
                }
            }
        });

        // Wait for the socket to be available
        for _ in 0..50 {
            if socket.exists() {
                break;
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        }

        if !socket.exists() {
            return Err("cloud-hypervisor socket not available after 5s".to_string());
        }

        let client = CloudHypervisorClient::new(socket_str);

        let vm_config = VmConfig {
            payload: PayloadConfig {
                kernel: None,
                cmdline: None,
                firmware: Some(self.config.firmware_path.clone()),
                initramfs: None,
            },
            cpus: Some(CpuConfig {
                boot_vcpus: vcpus,
                max_vcpus: vcpus,
            }),
            memory: Some(MemoryConfig {
                size: (memory_mb as u64) * 1024 * 1024,
            }),
            disks: Some(vec![DiskConfig {
                path: Self::path_str(&instance_image)?.to_string(),
                readonly: Some(false),
                image_type: None,
            }]),
            net: Some(vec![NetConfig {
                tap: None,
                ip: None,
                mask: None,
                mac: None,
            }]),
            serial: Some(ConsoleConfig {
                mode: "Tty".to_string(),
            }),
            console: Some(ConsoleConfig {
                mode: "Off".to_string(),
            }),
        };

        client
            .create_vm(&vm_config)
            .await
            .map_err(|e| format!("Failed to create VM: {}", e))?;

        client
            .boot_vm()
            .await
            .map_err(|e| format!("Failed to boot VM: {}", e))?;

        info!(
            "Cloud Hypervisor VM {} started for deployment {}",
            instance_id, deployment.id
        );

        Ok(())
    }

    /// Stop and remove a VM instance.
    async fn stop_vm(&self, instance_id: &str) -> bool {
        let socket = self.socket_path(instance_id);
        let socket_str = match socket.to_str() {
            Some(s) => s,
            None => return false,
        };

        if !socket.exists() {
            debug!("Socket {} does not exist, VM already stopped", socket_str);
            return true;
        }

        let client = CloudHypervisorClient::new(socket_str);

        if let Err(e) = client.shutdown_vm().await {
            warn!("Failed to shutdown VM {}: {}", instance_id, e);
        }

        if let Err(e) = client.delete_vm().await {
            warn!("Failed to delete VM {}: {}", instance_id, e);
        }

        if let Err(e) = tokio::fs::remove_file(&socket).await {
            debug!("Failed to remove socket {}: {}", socket_str, e);
        }

        let instance_image = self.instance_image_path(instance_id);
        if let Err(e) = tokio::fs::remove_file(&instance_image).await {
            debug!("Failed to remove instance image {:?}: {}", instance_image, e);
        }

        info!("Cloud Hypervisor VM {} stopped", instance_id);
        true
    }
}

fn parse_resources(deployment: &Deployment) -> (u32, u32) {
    let mut vcpus = 1u32;
    let mut memory_mb = 256u32;

    if let Some(ref resources) = deployment.resources {
        if let Some(ref limits) = resources.limits {
            if let Some(ref cpu) = limits.cpu {
                if let Ok(nano) = crate::models::deployments::parse_cpu_string(cpu) {
                    vcpus = std::cmp::max(1, (nano / 1_000_000_000) as u32);
                }
            }
            if let Some(ref mem) = limits.memory {
                if let Ok(bytes) = crate::models::deployments::parse_memory_string(mem) {
                    memory_mb = std::cmp::max(128, (bytes / (1024 * 1024)) as u32);
                }
            }
        }
    }

    (vcpus, memory_mb)
}

#[async_trait]
impl RuntimeLifecycle for CloudHypervisorLifecycle {
    async fn apply(
        &self,
        mut deployment: Deployment,
        resolved_mounts: Vec<ResolvedMount>,
    ) -> Deployment {
        if deployment.status == DeploymentStatus::Deleted {
            let instances = self.scan_instances(&deployment.id, &[]).await;
            for instance_id in &instances {
                if !self.stop_vm(instance_id).await {
                    warn!("Failed to stop VM {} during deletion", instance_id);
                }
            }
            deployment.instances.clear();
            return deployment;
        }

        // Refresh instances from disk
        deployment.instances = self
            .scan_instances(&deployment.id, &["Running", "Created", "Booting"])
            .await;

        let current_count = deployment.instances.len();
        let target_count = deployment.replicas as usize;

        if current_count < target_count {
            let instance_id = format!(
                "ch-{}-{}",
                &deployment.id[..8.min(deployment.id.len())],
                crate::runtime::docker::tiny_id()
            );

            match self.start_vm_process(&instance_id, &deployment).await {
                Ok(()) => {
                    deployment.instances.push(instance_id);
                    if deployment.status == DeploymentStatus::Creating
                        || deployment.status == DeploymentStatus::Pending
                    {
                        deployment.status = DeploymentStatus::Running;
                    }
                }
                Err(e) => {
                    error!(
                        "Failed to start Cloud Hypervisor VM for deployment {}: {}",
                        deployment.id, e
                    );
                    deployment.restart_count += 1;
                    deployment.status = DeploymentStatus::Failed;
                }
            }
        } else if current_count > target_count {
            if let Some(instance_id) = deployment.instances.first().cloned() {
                if !self.stop_vm(&instance_id).await {
                    warn!("Failed to stop VM {} during scale down", instance_id);
                }
                deployment.instances.remove(0);
            }
        }

        let _ = &resolved_mounts; // TODO: mount volumes via virtio-fs
        deployment
    }

    async fn list_instances(&self, deployment_id: String, _status: &str) -> Vec<String> {
        self.scan_instances(&deployment_id, &["Running"]).await
    }

    async fn remove_instance(&self, instance_id: String) -> bool {
        self.stop_vm(&instance_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_resources_defaults() {
        let deployment = Deployment {
            id: "test".to_string(),
            created_at: String::new(),
            updated_at: None,
            status: DeploymentStatus::Creating,
            restart_count: 0,
            namespace: "default".to_string(),
            name: "test".to_string(),
            image: "nginx:latest".to_string(),
            config: None,
            runtime: "cloud-hypervisor".to_string(),
            kind: "worker".to_string(),
            replicas: 1,
            command: vec![],
            instances: vec![],
            labels: std::collections::HashMap::new(),
            environment: std::collections::HashMap::new(),
            volumes: "[]".to_string(),
            health_checks: vec![],
            resources: None,
            image_digest: None,
            pending_events: vec![],
            parent_id: None,
        };

        let (vcpus, memory_mb) = parse_resources(&deployment);
        assert_eq!(vcpus, 1);
        assert_eq!(memory_mb, 256);
    }

    #[test]
    fn parse_resources_with_limits() {
        let deployment = Deployment {
            id: "test".to_string(),
            created_at: String::new(),
            updated_at: None,
            status: DeploymentStatus::Creating,
            restart_count: 0,
            namespace: "default".to_string(),
            name: "test".to_string(),
            image: "nginx:latest".to_string(),
            config: None,
            runtime: "cloud-hypervisor".to_string(),
            kind: "worker".to_string(),
            replicas: 1,
            command: vec![],
            instances: vec![],
            labels: std::collections::HashMap::new(),
            environment: std::collections::HashMap::new(),
            volumes: "[]".to_string(),
            health_checks: vec![],
            resources: Some(crate::models::deployments::Resource {
                limits: Some(crate::models::deployments::ResourceSpec {
                    cpu: Some("2".to_string()),
                    memory: Some("512Mi".to_string()),
                }),
                requests: None,
            }),
            image_digest: None,
            pending_events: vec![],
            parent_id: None,
        };

        let (vcpus, memory_mb) = parse_resources(&deployment);
        assert_eq!(vcpus, 2);
        assert_eq!(memory_mb, 512);
    }

    #[test]
    fn deployment_prefix_format() {
        assert_eq!(
            CloudHypervisorLifecycle::deployment_prefix("abcdef12-3456-7890"),
            "ch-abcdef12-"
        );
    }
}
