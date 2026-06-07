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
use crate::hypervisor::cloud_init::GuestNet;
use crate::hypervisor::error::RuntimeError;
use crate::hypervisor::host_net::InstanceNet;
use crate::hypervisor::lifecycle_trait::RuntimeLifecycle;
use crate::hypervisor::port_forwarder::{self, PortForwarder};
use crate::hypervisor::tap::TapDevice;
use crate::models::deployments::{Deployment, DeploymentStatus};
use crate::models::volume::ResolvedMount;
use crate::runtime::docker::tiny_id;
use crate::runtime::firecracker::client::{
    BootSource, Drive, FirecrackerClient, MachineConfig, NetworkInterface,
};
use async_trait::async_trait;
use std::collections::HashMap;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
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
    /// Live host tap devices, keyed by instance id. Unlike Cloud Hypervisor,
    /// Firecracker doesn't create the tap itself — Ring owns its whole
    /// lifecycle. Dropping the entry deletes the interface from the host.
    taps: Mutex<HashMap<String, TapDevice>>,
    /// Live socat port-forwarders, keyed by instance id. Dropping the entry
    /// kills the socat process.
    port_forwarders: Mutex<HashMap<String, Vec<PortForwarder>>>,
}

impl FirecrackerLifecycle {
    pub fn new(config: FirecrackerRuntimeConfig) -> Self {
        Self {
            config,
            pids: Mutex::new(HashMap::new()),
            taps: Mutex::new(HashMap::new()),
            port_forwarders: Mutex::new(HashMap::new()),
        }
    }

    fn socket_path(&self, instance_id: &str) -> String {
        format!("{}/{}.sock", self.config.socket_dir, instance_id)
    }

    fn rootfs_path(&self, instance_id: &str) -> String {
        format!("{}/{}.ext4", self.config.socket_dir, instance_id)
    }

    /// Per-instance serial console log. Firecracker writes the guest's ttyS0
    /// (kernel + init + service output) to the process stdout; we persist it so
    /// boot/runtime issues are diagnosable and log shippers can tail it.
    fn console_log_path(&self, instance_id: &str) -> String {
        format!("{}/{}.console.log", self.config.socket_dir, instance_id)
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

        // Persist the guest serial console (stdout) to a per-instance file so
        // boot/runtime issues are diagnosable and log shippers can tail it.
        // Falls back to null if the file can't be opened — never block boot on
        // logging. stderr (firecracker's own diagnostics) shares the same file.
        let console_log = self.console_log_path(&instance_id);
        let (out, err): (std::process::Stdio, std::process::Stdio) =
            match std::fs::File::create(&console_log) {
                Ok(f) => match f.try_clone() {
                    Ok(f2) => (f.into(), f2.into()),
                    Err(_) => (f.into(), std::process::Stdio::null()),
                },
                Err(e) => {
                    warn!("could not open console log {}: {}", console_log, e);
                    (std::process::Stdio::null(), std::process::Stdio::null())
                }
            };

        // Spawn the firecracker process bound to its API socket.
        let child = std::process::Command::new(&self.config.binary_path)
            .arg("--api-sock")
            .arg(&socket_path)
            .stdout(out)
            .stderr(err)
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
            self.kill_pid(pid).await;
            let _ = std::fs::remove_file(&rootfs_rw);
            return Err(RuntimeError::VmStartFailed(
                "firecracker API socket never appeared".to_string(),
            ));
        }

        // If the deployment publishes any port, allocate a deterministic /30
        // and create the host tap. Unlike Cloud Hypervisor, Firecracker does
        // not create the tap — Ring creates it here (held in `tap` so an early
        // return on any later error deletes it via Drop) and hands its name to
        // Firecracker, while cloud-init configures the matching guest IP.
        let net_alloc = if deployment.ports.is_empty() {
            None
        } else {
            Some(InstanceNet::for_instance(&instance_id))
        };
        let tap = match &net_alloc {
            Some(n) => match TapDevice::create(&n.tap_name, &n.host_ip, n.prefix_len) {
                Ok(t) => Some(t),
                Err(e) => {
                    self.kill_pid(pid).await;
                    let _ = std::fs::remove_file(&socket_path);
                    let _ = std::fs::remove_file(&rootfs_rw);
                    return Err(e);
                }
            },
            None => None,
        };

        // Configure + boot via the REST API (the spike's PUT sequence, plus a
        // network interface and a cidata drive when applicable).
        let client = FirecrackerClient::new(&socket_path);
        let boot = self
            .configure_and_boot(
                &client,
                &instance_id,
                &rootfs_rw,
                deployment,
                net_alloc.as_ref(),
            )
            .await;
        if let Err(e) = boot {
            // `tap` drops here, deleting the interface.
            self.kill_pid(pid).await;
            let _ = std::fs::remove_file(&socket_path);
            let _ = std::fs::remove_file(&rootfs_rw);
            let _ = std::fs::remove_file(self.cidata_path(&instance_id));
            return Err(RuntimeError::VmStartFailed(format!(
                "configure/boot failed for {}: {}",
                instance_id, e
            )));
        }

        // Spawn one socat per declared port now the guest is up. A bind race
        // (port taken between the pre-check and now) tears the VM down rather
        // than leaving a black-hole port. `forwarders` is owned locally; its
        // Drop kills any socat already spawned on early return.
        if let Some(n) = &net_alloc {
            let mut forwarders = Vec::with_capacity(deployment.ports.len());
            for p in &deployment.ports {
                match port_forwarder::spawn_forwarder(
                    &n.guest_ip,
                    p.published,
                    p.target,
                    p.host_ip.as_deref(),
                    p.protocol,
                )
                .await
                {
                    Ok(fw) => forwarders.push(fw),
                    Err(e) => {
                        let _ = client.send_ctrl_alt_del().await;
                        self.kill_pid(pid).await;
                        let _ = std::fs::remove_file(&socket_path);
                        let _ = std::fs::remove_file(&rootfs_rw);
                        let _ = std::fs::remove_file(self.cidata_path(&instance_id));
                        return Err(e);
                    }
                }
            }
            if !forwarders.is_empty() {
                self.port_forwarders
                    .lock()
                    .unwrap()
                    .insert(instance_id.clone(), forwarders);
            }
        }

        self.pids.lock().unwrap().insert(instance_id.clone(), pid);
        if let Some(t) = tap {
            self.taps.lock().unwrap().insert(instance_id.clone(), t);
        }
        info!("Firecracker microVM {} booted (pid {})", instance_id, pid);
        Ok(instance_id)
    }

    fn cidata_path(&self, instance_id: &str) -> String {
        format!("{}/{}.cidata.iso", self.config.socket_dir, instance_id)
    }

    async fn configure_and_boot(
        &self,
        client: &FirecrackerClient,
        instance_id: &str,
        rootfs_rw: &str,
        deployment: &Deployment,
        net_alloc: Option<&InstanceNet>,
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

        // A cidata ISO is attached whenever cloud-init has something to do:
        // env vars or a static network config. Mounts are not wired yet.
        let guest_net = net_alloc.map(|n| GuestNet {
            guest_ip: n.guest_ip.clone(),
            host_ip: n.host_ip.clone(),
            prefix_len: n.prefix_len,
            mac: n.mac.clone(),
        });
        if !deployment.environment.is_empty() || guest_net.is_some() {
            let socket_dir = PathBuf::from(&self.config.socket_dir);
            let iso_path = crate::hypervisor::cloud_init::build_cidata_iso(
                instance_id,
                deployment,
                &[],
                guest_net.as_ref(),
                &socket_dir,
            )
            .await
            .map_err(|e| e.to_string())?;
            client
                .put_drive(&Drive {
                    drive_id: "cidata".to_string(),
                    path_on_host: iso_path.to_string_lossy().to_string(),
                    is_root_device: false,
                    is_read_only: true,
                })
                .await
                .map_err(|e| e.to_string())?;
        }

        // Attach the network interface (the tap already exists on the host).
        if let Some(n) = net_alloc {
            client
                .put_network_interface(&NetworkInterface {
                    iface_id: "eth0".to_string(),
                    host_dev_name: n.tap_name.clone(),
                    guest_mac: Some(n.mac.clone()),
                })
                .await
                .map_err(|e| e.to_string())?;
        }

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

    /// Tear down one instance: kill the socat forwarders, gracefully shut the
    /// guest, kill the process, delete the host tap, and unlink the socket,
    /// rootfs copy and cidata ISO. Returns true if the instance is gone after.
    async fn stop_vm(&self, instance_id: &str) -> bool {
        let socket_path = self.socket_path(instance_id);

        // Drop the port-forwarders first so nothing still routes to the guest.
        self.port_forwarders.lock().unwrap().remove(instance_id);

        // Best-effort graceful shutdown if the socket is still live.
        if Path::new(&socket_path).exists() {
            let client = FirecrackerClient::new(&socket_path);
            let _ = client.send_ctrl_alt_del().await;
            tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;
        }

        // Kill the firecracker process. The PID lives in `pids` for instances
        // this process booted; after a ring-server restart the map is empty, so
        // fall back to finding the process by its `--api-sock` argument in
        // /proc. Firecracker has no remote "delete VM" — killing the process is
        // the only way to stop it — so this fallback is what makes teardown
        // survive a restart.
        let pid = self
            .pids
            .lock()
            .unwrap()
            .remove(instance_id)
            .or_else(|| find_pid_by_socket(&socket_path));
        if let Some(pid) = pid {
            self.kill_pid(pid).await;
        }

        // Delete the host tap. For instances we booted it's in `taps` and its
        // Drop runs TapDevice::delete. After a restart the map is empty, so
        // re-derive the tap from the instance id (the name is a pure function of
        // it) and delete it directly — otherwise the interface leaks on the
        // host. The VM process is already dead (kill_pid waited), so the tap's
        // backend is free. Harmless if the instance never had a tap: delete just
        // fails to re-attach and no-ops.
        if self.taps.lock().unwrap().remove(instance_id).is_none() {
            let name = InstanceNet::for_instance(instance_id).tap_name;
            TapDevice::adopt(&name).delete();
        }

        let _ = std::fs::remove_file(&socket_path);
        let _ = std::fs::remove_file(self.rootfs_path(instance_id));
        let _ = std::fs::remove_file(self.cidata_path(instance_id));
        let _ = std::fs::remove_file(self.console_log_path(instance_id));
        !Path::new(&socket_path).exists()
    }

    /// SIGTERM the firecracker process, then SIGKILL if it doesn't exit
    /// promptly, and wait until it's actually gone. Waiting matters for the
    /// tap: Firecracker holds the tap's backend fd while alive, so the tap
    /// can only be removed once the process has fully exited.
    async fn kill_pid(&self, pid: u32) {
        let pid_i = pid as i32;
        unsafe { libc::kill(pid_i, libc::SIGTERM) };
        for i in 0..20 {
            // kill(pid, 0) returns -1/ESRCH once the process is gone.
            if unsafe { libc::kill(pid_i, 0) } != 0 {
                return;
            }
            if i == 5 {
                // Still alive after ~300ms — escalate.
                unsafe { libc::kill(pid_i, libc::SIGKILL) };
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        }
    }

    /// Instance ids of a deployment whose API socket still exists on disk.
    /// The `.sock` file is the source of truth for "running", scanned from
    /// `socket_dir` rather than the in-memory `pids` map — so instances survive
    /// a `ring-server` restart (after which the maps are empty but the VMs, and
    /// their sockets, are still there). Mirrors Cloud Hypervisor's disk scan.
    fn scan_instances(&self, deployment_id: &str) -> Vec<String> {
        let prefix = format!("{}-", deployment_id);
        let mut instances = Vec::new();
        let entries = match std::fs::read_dir(&self.config.socket_dir) {
            Ok(e) => e,
            Err(_) => return instances,
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if let Some(id) = name.strip_suffix(".sock")
                && id.starts_with(&prefix)
            {
                instances.push(id.to_string());
            }
        }
        instances
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

/// Find the PID of the `firecracker` process bound to `socket_path`, by scanning
/// `/proc/<pid>/cmdline` for one whose `--api-sock` argument matches. Used as a
/// teardown fallback after a `ring-server` restart, when the PID is no longer in
/// the in-memory map but the VM (and its socket) is still alive. Returns `None`
/// if no live process references that socket (already gone, or never existed).
fn find_pid_by_socket(socket_path: &str) -> Option<u32> {
    for entry in std::fs::read_dir("/proc").ok()?.flatten() {
        // Only numeric entries are processes.
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        let Ok(cmdline) = std::fs::read(format!("/proc/{}/cmdline", pid)) else {
            continue;
        };
        if cmdline_matches_socket(&cmdline, socket_path) {
            return Some(pid);
        }
    }
    None
}

/// Does this `/proc/<pid>/cmdline` (NUL-separated argv) belong to a
/// `firecracker` process bound to `socket_path`? True iff argv[0] ends with
/// `firecracker` and some later argument equals the socket path exactly — the
/// exact match stops `/x/a.sock` from matching `/x/a.sock.bak`.
fn cmdline_matches_socket(cmdline: &[u8], socket_path: &str) -> bool {
    let mut args = cmdline.split(|&b| b == 0);
    if args.next().map(|a0| a0.ends_with(b"firecracker")) != Some(true) {
        return false;
    }
    args.any(|arg| arg == socket_path.as_bytes())
}

/// Parse vCPU count + memory (MiB) from the deployment's resource limits
/// (falling back to requests). vCPUs round up from a fractional CPU quantity to
/// at least 1; memory falls back to a sane floor so a microVM has room to run a
/// real service rather than OOMing at boot.
fn parse_resources(deployment: &Deployment) -> (u32, u32) {
    use crate::models::deployments::{parse_cpu_string, parse_memory_string};

    // Minimum that boots systemd + a typical service without OOM. 128 MiB is
    // enough for the kernel + init but starves php-fpm/most runtimes.
    const DEFAULT_MEM_MIB: u32 = 512;
    const DEFAULT_VCPUS: u32 = 1;

    let spec = deployment
        .resources
        .as_ref()
        .and_then(|r| r.limits.as_ref().or(r.requests.as_ref()));

    let mem_mib = spec
        .and_then(|s| s.memory.as_ref())
        .and_then(|m| parse_memory_string(m).ok())
        .map(|bytes| (bytes / (1024 * 1024)).max(1) as u32)
        .filter(|&m| m >= 64)
        .unwrap_or(DEFAULT_MEM_MIB);

    // parse_cpu_string returns nano-CPUs (1_000_000_000 = 1 vCPU); round up to
    // whole vCPUs since Firecracker can't allocate fractional cores.
    const NANO_PER_VCPU: i64 = 1_000_000_000;
    let vcpus = spec
        .and_then(|s| s.cpu.as_ref())
        .and_then(|c| parse_cpu_string(c).ok())
        .map(|nanocpu| ((nanocpu + NANO_PER_VCPU - 1) / NANO_PER_VCPU).max(1) as u32)
        .unwrap_or(DEFAULT_VCPUS);

    (vcpus, mem_mib)
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

    /// The guest IP is a pure function of the instance id (same allocation as
    /// at boot), so TCP/HTTP health probes can reach the workload without any
    /// persistent state. Returns `None` for instances without a network (no
    /// published ports) — there's no reachable address to probe.
    async fn instance_address(&self, instance_id: &str) -> Option<IpAddr> {
        // An instance has a reachable IP iff it allocated a tap. That's tracked
        // in `taps` for instances we booted; after a ring-server restart the map
        // is empty, so fall back to checking the host for the tap interface
        // (its name is a pure function of the instance id). Either source means
        // "has a network" → return the deterministic guest IP.
        let net = InstanceNet::for_instance(instance_id);
        let has_tap =
            self.taps.lock().unwrap().contains_key(instance_id) || TapDevice::exists(&net.tap_name);
        if !has_tap {
            return None;
        }
        net.guest_ip.parse().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_instances_reads_disk_not_memory() {
        // Post-restart simulation: sockets exist on disk, `pids` is empty.
        // scan_instances must still find the instances (it scans socket_dir),
        // otherwise a restarted ring-server would lose track of running VMs.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("ring-fc-scan-{}-{}", std::process::id(), nanos));
        std::fs::create_dir_all(&dir).unwrap();

        let cfg = FirecrackerRuntimeConfig {
            socket_dir: dir.to_string_lossy().to_string(),
            ..FirecrackerRuntimeConfig::default()
        };
        let lc = FirecrackerLifecycle::new(cfg);

        // Two sockets for our deployment, one for another, plus noise.
        for f in [
            "dep-1-aaa.sock",
            "dep-1-bbb.sock",
            "dep-2-ccc.sock",
            "dep-1-aaa.ext4", // not a socket
            "dep-1.txt",
        ] {
            std::fs::write(dir.join(f), b"").unwrap();
        }

        // pids is empty (as after a restart).
        assert!(lc.pids.lock().unwrap().is_empty());

        let mut found = lc.scan_instances("dep-1");
        found.sort();
        assert_eq!(
            found,
            vec!["dep-1-aaa".to_string(), "dep-1-bbb".to_string()]
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn cmdline_matches_socket_exact_arg() {
        let sock = "/run/fc/dep-1-aaa.sock";
        // argv[0]=firecracker, then --api-sock <sock>
        let cmd = b"/usr/bin/firecracker\0--api-sock\0/run/fc/dep-1-aaa.sock\0";
        assert!(cmdline_matches_socket(cmd, sock));
    }

    #[test]
    fn cmdline_matches_socket_rejects_prefix_collision() {
        // A different VM whose socket merely starts with ours must not match.
        let sock = "/run/fc/dep-1-aaa.sock";
        let cmd = b"/usr/bin/firecracker\0--api-sock\0/run/fc/dep-1-aaa.sock.bak\0";
        assert!(!cmdline_matches_socket(cmd, sock));
    }

    #[test]
    fn cmdline_matches_socket_rejects_non_firecracker() {
        // Right socket arg, wrong process — must not match.
        let sock = "/run/fc/dep-1-aaa.sock";
        let cmd = b"/usr/bin/socat\0--api-sock\0/run/fc/dep-1-aaa.sock\0";
        assert!(!cmdline_matches_socket(cmd, sock));
    }

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
