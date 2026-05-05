use super::client::{
    CloudHypervisorClient, ConsoleConfig, CpuConfig, DiskConfig, FsConfig, MemoryConfig, NetConfig,
    PayloadConfig, VmConfig,
};
use crate::models::deployments::{Deployment, DeploymentStatus, MAX_RESTART_COUNT};
use crate::models::volume::ResolvedMount;
use crate::runtime::error::RuntimeError;
use crate::runtime::host_net::InstanceNet;
use crate::runtime::lifecycle_trait::RuntimeLifecycle;
use crate::runtime::port_forwarder::{self, PortForwarder};
use crate::runtime::virtiofs::{self, VirtiofsMount};
use async_trait::async_trait;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
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
    /// Forwarded to `cloud-hypervisor --seccomp <value>` when set. None means
    /// CH applies its own default (kill on violation).
    pub seccomp: Option<String>,
}

impl Default for CloudHypervisorRuntimeConfig {
    fn default() -> Self {
        let base_dir = crate::config::config::get_config_dir();
        Self {
            binary_path: "cloud-hypervisor".to_string(),
            firmware_path: format!("{}/cloud-hypervisor/vmlinux", base_dir),
            socket_dir: format!("{}/cloud-hypervisor/sockets", base_dir),
            seccomp: None,
        }
    }
}

impl CloudHypervisorRuntimeConfig {
    /// Merge a user-facing config section with the defaults. Any field left
    /// unset in `user` falls back to `CloudHypervisorRuntimeConfig::default`.
    pub(crate) fn from_user_config(user: &crate::config::config::CloudHypervisorConfig) -> Self {
        let defaults = Self::default();
        Self {
            binary_path: user.binary_path.clone().unwrap_or(defaults.binary_path),
            firmware_path: user.firmware_path.clone().unwrap_or(defaults.firmware_path),
            socket_dir: user.socket_dir.clone().unwrap_or(defaults.socket_dir),
            seccomp: user.seccomp.clone(),
        }
    }
}

pub struct CloudHypervisorLifecycle {
    config: CloudHypervisorRuntimeConfig,
    /// Live virtiofsd processes, keyed by VM instance id. The daemon stays
    /// up as long as the VM is running; dropping the entry kills the daemon
    /// and unlinks its socket. Wrapped in a sync `Mutex` because it is only
    /// touched briefly to insert/remove — never across `.await`.
    virtiofs_mounts: Mutex<HashMap<String, Vec<VirtiofsMount>>>,
    /// Live socat port-forwarders, keyed by VM instance id. Same lifetime
    /// rules as `virtiofs_mounts`: dropping the entry kills the socat.
    port_forwarders: Mutex<HashMap<String, Vec<PortForwarder>>>,
}

impl CloudHypervisorLifecycle {
    pub fn new(config: CloudHypervisorRuntimeConfig) -> Self {
        Self {
            config,
            virtiofs_mounts: Mutex::new(HashMap::new()),
            port_forwarders: Mutex::new(HashMap::new()),
        }
    }

    fn instance_share_dir(&self, instance_id: &str) -> PathBuf {
        PathBuf::from(&self.config.socket_dir).join(format!("{}.shares", instance_id))
    }

    fn virtiofs_socket_path(&self, instance_id: &str, idx: usize) -> PathBuf {
        PathBuf::from(&self.config.socket_dir)
            .join(format!("{}.virtiofs-{}.sock", instance_id, idx))
    }

    fn named_volume_dir(&self, namespace: &str, name: &str) -> PathBuf {
        PathBuf::from(&self.config.socket_dir)
            .join("volumes")
            .join(namespace)
            .join(name)
    }

    fn socket_path(&self, instance_id: &str) -> PathBuf {
        PathBuf::from(&self.config.socket_dir).join(format!("{}.sock", instance_id))
    }

    fn instance_image_path(&self, instance_id: &str) -> PathBuf {
        PathBuf::from(&self.config.socket_dir).join(format!("{}.raw", instance_id))
    }

    fn cidata_iso_path(&self, instance_id: &str) -> PathBuf {
        PathBuf::from(&self.config.socket_dir).join(format!("{}.cidata.iso", instance_id))
    }

    fn console_log_path(&self, instance_id: &str) -> PathBuf {
        PathBuf::from(&self.config.socket_dir).join(format!("{}.console.log", instance_id))
    }

    /// Public for the e2e test only — production callers go through
    /// `get_logs` / `stream_logs`.
    #[cfg(test)]
    pub(crate) fn console_log_path_for_test(&self, instance_id: &str) -> PathBuf {
        self.console_log_path(instance_id)
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

    fn path_str(path: &Path) -> Result<&str, RuntimeError> {
        path.to_str().ok_or_else(|| {
            RuntimeError::Other(format!("Path contains non-UTF-8 characters: {:?}", path))
        })
    }

    /// Prepare the host side of every requested volume and spawn the
    /// matching virtiofsd processes. Returns the live mounts plus their
    /// `FsConfig` ready to attach to a `VmConfig`.
    ///
    /// Mapping:
    /// - `Bind`    → virtiofsd directly on the user-supplied host path.
    /// - `Named`   → virtiofsd on `<socket_dir>/volumes/<namespace>/<name>`,
    ///   created on first use, persisted across deployment lifetimes.
    /// - `Content` → write the rendered file under
    ///   `<socket_dir>/<instance>.shares/cfg-<idx>/<basename>` and virtiofsd
    ///   on that directory; the guest mounts the directory at the parent of
    ///   the requested destination so the file lands at the right path.
    async fn prepare_virtiofs_mounts(
        &self,
        instance_id: &str,
        namespace: &str,
        resolved: &[ResolvedMount],
    ) -> Result<(Vec<VirtiofsMount>, Vec<FsConfig>), RuntimeError> {
        if resolved.is_empty() {
            return Ok((Vec::new(), Vec::new()));
        }

        let virtiofsd_path = virtiofs::locate_virtiofsd().ok_or_else(|| {
            RuntimeError::Other(
                "virtiofsd binary not found (install virtiofsd or set RING_VIRTIOFSD)".to_string(),
            )
        })?;

        let share_root = self.instance_share_dir(instance_id);
        if share_root.exists() {
            let _ = tokio::fs::remove_dir_all(&share_root).await;
        }
        tokio::fs::create_dir_all(&share_root)
            .await
            .map_err(RuntimeError::Io)?;

        let mut mounts: Vec<VirtiofsMount> = Vec::with_capacity(resolved.len());
        let mut fs_configs: Vec<FsConfig> = Vec::with_capacity(resolved.len());

        for (idx, m) in resolved.iter().enumerate() {
            let socket_path = self.virtiofs_socket_path(instance_id, idx);

            let (tag, source, destination, read_only) = match m {
                ResolvedMount::Bind {
                    source,
                    destination,
                    read_only,
                } => (
                    format!("bind-{}", idx),
                    PathBuf::from(source),
                    destination.clone(),
                    *read_only,
                ),
                ResolvedMount::Named {
                    name,
                    destination,
                    read_only,
                    driver: _,
                } => {
                    let dir = self.named_volume_dir(namespace, name);
                    tokio::fs::create_dir_all(&dir)
                        .await
                        .map_err(RuntimeError::Io)?;
                    (format!("vol-{}", idx), dir, destination.clone(), *read_only)
                }
                ResolvedMount::Content {
                    content,
                    destination,
                } => {
                    let dest_path = Path::new(destination);
                    let parent = dest_path
                        .parent()
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "/".to_string());
                    let basename = dest_path
                        .file_name()
                        .ok_or_else(|| {
                            RuntimeError::Other(format!(
                                "config volume destination has no filename: {}",
                                destination
                            ))
                        })?
                        .to_string_lossy()
                        .into_owned();

                    let cfg_dir = share_root.join(format!("cfg-{}", idx));
                    tokio::fs::create_dir_all(&cfg_dir)
                        .await
                        .map_err(RuntimeError::Io)?;
                    tokio::fs::write(cfg_dir.join(&basename), content)
                        .await
                        .map_err(RuntimeError::Io)?;
                    // Mount the *parent* directory inside the guest. The file
                    // we wrote shows up at `<parent>/<basename>` == the
                    // user-supplied destination.
                    (format!("cfg-{}", idx), cfg_dir, parent, true)
                }
            };

            let mount = virtiofs::spawn_virtiofsd(
                &virtiofsd_path,
                &source,
                &socket_path,
                &tag,
                &destination,
                read_only,
            )
            .await?;

            fs_configs.push(FsConfig {
                tag: mount.tag.clone(),
                socket: mount.socket_path_str()?.to_string(),
                // CH defaults from cloud-hypervisor --help.
                num_queues: 1,
                queue_size: 1024,
            });
            mounts.push(mount);
        }

        Ok((mounts, fs_configs))
    }

    /// Start the cloud-hypervisor process for a VM instance.
    ///
    /// Errors are typed so the caller can distinguish permanent failures
    /// (`FirmwareNotFound`, `ImageNotFound`) — which would loop forever if
    /// retried — from transient ones (`VmStartFailed`) that warrant a retry.
    async fn start_vm_process(
        &self,
        instance_id: &str,
        deployment: &Deployment,
        resolved_mounts: &[ResolvedMount],
    ) -> Result<(), RuntimeError> {
        // Permanent: missing firmware. The operator must fix `firmware_path`.
        let firmware = PathBuf::from(&self.config.firmware_path);
        if !firmware.exists() {
            return Err(RuntimeError::FirmwareNotFound(
                self.config.firmware_path.clone(),
            ));
        }

        let socket = self.socket_path(instance_id);
        let socket_str = Self::path_str(&socket)?;

        // Ensure socket directory exists
        tokio::fs::create_dir_all(&self.config.socket_dir)
            .await
            .map_err(|e| RuntimeError::Io(e))?;

        // Permanent: missing VM image. The operator must fix the deployment.
        let base_image = PathBuf::from(&deployment.image);
        if !base_image.exists() {
            return Err(RuntimeError::ImageNotFound(deployment.image.clone()));
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
                .map_err(|e| RuntimeError::Io(e))?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(RuntimeError::VmStartFailed(format!(
                    "Failed to copy VM image: {}",
                    stderr
                )));
            }
        }

        // Parse resource limits
        let (vcpus, memory_mb) = parse_resources(deployment);

        // Start cloud-hypervisor process. We capture stderr so that crashes
        // (e.g. seccomp violations, missing kvm caps, bad firmware) surface in
        // the Ring log instead of being silently dropped.
        let mut command = Command::new(&self.config.binary_path);
        command.arg("--api-socket").arg(socket_str);
        if let Some(seccomp) = &self.config.seccomp {
            command.arg("--seccomp").arg(seccomp);
        }
        let mut child = command
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                RuntimeError::VmStartFailed(format!("Failed to start cloud-hypervisor: {}", e))
            })?;

        let stderr = child.stderr.take();

        // Monitor the child process in the background
        let child_instance_id = instance_id.to_string();
        tokio::spawn(async move {
            let stderr_task = stderr.map(|mut s| {
                tokio::spawn(async move {
                    use tokio::io::AsyncReadExt;
                    let mut buf = Vec::new();
                    let _ = s.read_to_end(&mut buf).await;
                    String::from_utf8_lossy(&buf).into_owned()
                })
            });

            match child.wait().await {
                Ok(status) => {
                    let stderr_output = match stderr_task {
                        Some(handle) => handle.await.unwrap_or_default(),
                        None => String::new(),
                    };
                    if status.success() {
                        debug!(
                            "cloud-hypervisor process for {} exited cleanly",
                            child_instance_id
                        );
                    } else {
                        error!(
                            "cloud-hypervisor process for {} exited with {} — stderr: {}",
                            child_instance_id,
                            status,
                            stderr_output.trim()
                        );
                    }
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
            return Err(RuntimeError::VmStartFailed(
                "cloud-hypervisor socket not available after 5s".to_string(),
            ));
        }

        let client = CloudHypervisorClient::new(socket_str);

        // Spawn virtiofsd for every requested volume *before* the cidata ISO
        // gets built, so cloud-init learns about the mounts in the same pass.
        // The mounts are stashed in `self.virtiofs_mounts` only after the VM
        // has booted successfully — until then, an early return drops them
        // and Drop kills the daemons.
        let (live_mounts, fs_configs) = self
            .prepare_virtiofs_mounts(instance_id, &deployment.namespace, resolved_mounts)
            .await
            .map_err(|e| {
                RuntimeError::VmStartFailed(format!("Failed to set up virtio-fs: {}", e))
            })?;

        let guest_mounts: Vec<super::cloud_init::GuestMount> = live_mounts
            .iter()
            .map(|m| super::cloud_init::GuestMount {
                tag: m.tag.clone(),
                destination: m.destination.clone(),
                read_only: m.read_only,
            })
            .collect();

        // If the deployment publishes any port, allocate a deterministic /30
        // for this VM and tell both CH and cloud-init what to do with it.
        // CH creates the tap and brings up its host-side IP; cloud-init
        // configures the matching guest-side IP at first boot.
        let needs_net = !deployment.ports.is_empty();
        let net_alloc = if needs_net {
            Some(InstanceNet::for_instance(instance_id))
        } else {
            None
        };
        let guest_net = net_alloc.as_ref().map(|n| super::cloud_init::GuestNet {
            guest_ip: n.guest_ip.clone(),
            host_ip: n.host_ip.clone(),
            prefix_len: n.prefix_len,
            mac: n.mac.clone(),
        });

        // Build the disk list. The main rootfs is always there. A cidata ISO
        // is attached whenever there's something for cloud-init to do —
        // env vars, virtio-fs mounts, or a static network config.
        let mut disks = vec![DiskConfig {
            path: Self::path_str(&instance_image)?.to_string(),
            readonly: Some(false),
            image_type: None,
        }];
        if !deployment.environment.is_empty() || !guest_mounts.is_empty() || guest_net.is_some() {
            let socket_dir = PathBuf::from(&self.config.socket_dir);
            let iso_path = super::cloud_init::build_cidata_iso(
                instance_id,
                deployment,
                &guest_mounts,
                guest_net.as_ref(),
                &socket_dir,
            )
            .await?;
            disks.push(DiskConfig {
                path: Self::path_str(&iso_path)?.to_string(),
                readonly: Some(true),
                image_type: None,
            });
        }

        let net_config = match &net_alloc {
            Some(n) => NetConfig {
                tap: Some(n.tap_name.clone()),
                ip: Some(n.host_ip.clone()),
                mask: Some(n.netmask.clone()),
                mac: Some(n.mac.clone()),
            },
            None => NetConfig {
                tap: None,
                ip: None,
                mask: None,
                mac: None,
            },
        };

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
                // vhost-user (virtio-fs) requires shared guest memory.
                // Off by default to avoid the small perf hit on VMs without volumes.
                shared: !fs_configs.is_empty(),
            }),
            disks: Some(disks),
            fs: if fs_configs.is_empty() {
                None
            } else {
                Some(fs_configs)
            },
            net: Some(vec![net_config]),
            // Append serial output to a per-instance file so `ring deployment
            // logs` can read it back. CH never rotates this file; cleanup is
            // tied to the VM lifetime via stop_vm. Anything the guest writes
            // to /dev/console (cloud-init banner, kernel messages, app stdout
            // when redirected) lands here.
            serial: Some(ConsoleConfig {
                mode: "File".to_string(),
                file: Some(Self::path_str(&self.console_log_path(instance_id))?.to_string()),
            }),
            console: Some(ConsoleConfig {
                mode: "Off".to_string(),
                file: None,
            }),
        };

        // From here on, any failure must clean up the half-created VM:
        // otherwise the socket and possibly a Created-state VM linger and
        // scan_instances counts them as live, which makes the scheduler
        // believe the deployment is satisfied and skip every retry. The
        // `live_mounts` are still owned locally; if we early-return below,
        // they drop here and the virtiofsd processes get killed.
        if let Err(e) = client.create_vm(&vm_config).await {
            self.stop_vm(instance_id).await;
            return Err(RuntimeError::VmStartFailed(format!(
                "Failed to create VM: {}",
                e
            )));
        }

        if let Err(e) = client.boot_vm().await {
            self.stop_vm(instance_id).await;
            return Err(RuntimeError::VmStartFailed(format!(
                "Failed to boot VM: {}",
                e
            )));
        }

        // VM is up — hand the mounts over to the lifecycle so they outlive
        // this function. The lock window is tiny and held only across an
        // insert.
        if !live_mounts.is_empty() {
            if let Ok(mut map) = self.virtiofs_mounts.lock() {
                map.insert(instance_id.to_string(), live_mounts);
            }
        }

        // Spawn one socat per declared port now that the guest IP is reachable
        // (cloud-init has had time to bring eth0 up). We don't fail the boot
        // if a port can't be forwarded — the VM is up; the caller can see
        // the missing port in `ring deployment events` if we emit one. For
        // now a warn! is enough to keep the scheduler from flapping.
        if let Some(net) = &net_alloc {
            let mut forwarders = Vec::with_capacity(deployment.ports.len());
            for p in &deployment.ports {
                match port_forwarder::spawn_forwarder(&net.guest_ip, p.published, p.target).await {
                    Ok(fw) => forwarders.push(fw),
                    Err(e) => {
                        warn!(
                            "Failed to publish port {}->{}:{} for VM {}: {}",
                            p.published, net.guest_ip, p.target, instance_id, e
                        );
                    }
                }
            }
            if !forwarders.is_empty() {
                if let Ok(mut map) = self.port_forwarders.lock() {
                    map.insert(instance_id.to_string(), forwarders);
                }
            }
        }

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
            debug!(
                "Failed to remove instance image {:?}: {}",
                instance_image, e
            );
        }

        // The cidata ISO is only present when the deployment shipped env vars
        // or virtio-fs mounts, but unconditional unlink is fine — missing-file
        // is logged at debug.
        let cidata_iso = self.cidata_iso_path(instance_id);
        if let Err(e) = tokio::fs::remove_file(&cidata_iso).await {
            debug!("Failed to remove cidata ISO {:?}: {}", cidata_iso, e);
        }

        let console_log = self.console_log_path(instance_id);
        if let Err(e) = tokio::fs::remove_file(&console_log).await {
            debug!("Failed to remove console log {:?}: {}", console_log, e);
        }

        // Drop any live virtiofsd processes for this instance. Their `Drop`
        // sends SIGKILL and unlinks the socket. Doing this *after* the VM is
        // shut down avoids the kernel logging spurious EIO when the guest
        // tries to flush a now-disconnected share.
        if let Ok(mut map) = self.virtiofs_mounts.lock() {
            let _ = map.remove(instance_id);
        }

        // Drop port-forwarders for this instance. Their `Drop` sends SIGKILL
        // to the matching socat processes and frees the listening ports.
        if let Ok(mut map) = self.port_forwarders.lock() {
            let _ = map.remove(instance_id);
        }

        // The per-instance share staging dir holds rendered config volumes;
        // remove it whether or not we had any virtio-fs mounts since last run.
        let share_dir = self.instance_share_dir(instance_id);
        if let Err(e) = tokio::fs::remove_dir_all(&share_dir).await {
            debug!("Failed to remove share dir {:?}: {}", share_dir, e);
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

        if current_count < target_count && deployment.status != DeploymentStatus::CrashLoopBackOff {
            let instance_id = format!(
                "ch-{}-{}",
                &deployment.id[..8.min(deployment.id.len())],
                crate::runtime::docker::tiny_id()
            );

            match self
                .start_vm_process(&instance_id, &deployment, &resolved_mounts)
                .await
            {
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

                    // Classify the failure: permanent errors (missing
                    // firmware/image) move the deployment to a terminal state
                    // immediately so the scheduler stops looping. Transient
                    // errors (process spawn, socket, API) increment
                    // restart_count and let the scheduler retry, capping at
                    // MAX_RESTART_COUNT to land in CrashLoopBackOff.
                    let (status, reason) = match &e {
                        RuntimeError::FirmwareNotFound(_) => {
                            (Some(DeploymentStatus::Failed), "FirmwareNotFound")
                        }
                        RuntimeError::ImageNotFound(_) => {
                            (Some(DeploymentStatus::ImagePullBackOff), "ImageNotFound")
                        }
                        _ => (None, "VmStartFailed"),
                    };

                    deployment.emit_event(
                        "error",
                        format!("{}", e),
                        "cloud-hypervisor",
                        Some(reason),
                    );

                    if let Some(terminal_status) = status {
                        deployment.status = terminal_status;
                    } else {
                        deployment.restart_count += 1;
                        if deployment.restart_count >= MAX_RESTART_COUNT {
                            deployment.status = DeploymentStatus::CrashLoopBackOff;
                        }
                        // The scheduler observes the bumped restart_count and
                        // arms the backoff window for the next cycle.
                    }
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

        deployment
    }

    async fn list_instances(&self, deployment_id: String, _status: &str) -> Vec<String> {
        self.scan_instances(&deployment_id, &["Running"]).await
    }

    async fn remove_instance(&self, instance_id: String) -> bool {
        self.stop_vm(&instance_id).await
    }

    async fn get_logs(
        &self,
        deployment_id: &str,
        tail: Option<&str>,
        since: Option<i32>,
        instance_filter: Option<&str>,
    ) -> Vec<crate::runtime::lifecycle_trait::Log> {
        // Read every state CH knows about: a crashed VM still has a console
        // log file the operator wants to see, even after the VM is gone.
        let instances = self.scan_instances(deployment_id, &[]).await;

        let mut logs = Vec::new();
        for instance_id in instances {
            if let Some(want) = instance_filter
                && instance_id != want
            {
                continue;
            }
            let path = self.console_log_path(&instance_id);
            let lines = super::console_logs::read_lines(&path, tail, since).await;
            for message in lines {
                logs.push(crate::runtime::lifecycle_trait::Log {
                    instance: instance_id.clone(),
                    level: crate::runtime::lifecycle_trait::classify_log(&message),
                    timestamp: crate::runtime::lifecycle_trait::extract_date(&message),
                    message,
                });
            }
        }
        logs
    }

    async fn stream_logs(
        &self,
        deployment_id: &str,
        tail: Option<&str>,
        since: Option<i32>,
        instance_filter: Option<&str>,
    ) -> std::pin::Pin<
        Box<
            dyn futures::Stream<Item = Result<axum::response::sse::Event, std::convert::Infallible>>
                + Send,
        >,
    > {
        use futures::stream::{self, StreamExt};

        let instances = self.scan_instances(deployment_id, &[]).await;
        let filtered: Vec<String> = instances
            .into_iter()
            .filter(|id| match instance_filter {
                Some(want) => id == want,
                None => true,
            })
            .collect();

        if filtered.is_empty() {
            return Box::pin(stream::empty());
        }

        let mut streams: Vec<
            std::pin::Pin<
                Box<
                    dyn futures::Stream<
                            Item = Result<axum::response::sse::Event, std::convert::Infallible>,
                        > + Send,
                >,
            >,
        > = Vec::new();

        for instance_id in filtered {
            let path = self.console_log_path(&instance_id);
            let owned_id = instance_id.clone();
            let raw =
                super::console_logs::stream_lines(path, tail.map(|s| s.to_string()), since).await;

            let mapped = raw.map(move |line| {
                let log = crate::runtime::lifecycle_trait::Log {
                    instance: owned_id.clone(),
                    level: crate::runtime::lifecycle_trait::classify_log(&line),
                    timestamp: crate::runtime::lifecycle_trait::extract_date(&line),
                    message: line,
                };
                let json = serde_json::to_string(&log).unwrap_or_default();
                Ok(axum::response::sse::Event::default().data(json))
            });

            streams.push(Box::pin(mapped));
        }

        Box::pin(stream::select_all(streams))
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
            ports: vec![],
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
            ports: vec![],
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

    fn lifecycle_with_socket_dir() -> (CloudHypervisorLifecycle, PathBuf) {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let socket_dir =
            std::env::temp_dir().join(format!("ring-ch-virtiofs-{}-{}", std::process::id(), nanos));
        std::fs::create_dir_all(&socket_dir).unwrap();
        let cfg = CloudHypervisorRuntimeConfig {
            binary_path: "cloud-hypervisor".to_string(),
            firmware_path: "/tmp/fake-fw".to_string(),
            socket_dir: socket_dir.to_string_lossy().into_owned(),
            seccomp: None,
        };
        (CloudHypervisorLifecycle::new(cfg), socket_dir)
    }

    fn skip_if_no_virtiofsd(test: &str) -> bool {
        if crate::runtime::virtiofs::locate_virtiofsd().is_none() {
            eprintln!(
                "skipping {}: virtiofsd not installed (apt install virtiofsd)",
                test
            );
            return true;
        }
        false
    }

    #[tokio::test]
    async fn prepare_mounts_empty_returns_nothing() {
        // Pure logic — no virtiofsd needed.
        let (lc, dir) = lifecycle_with_socket_dir();
        let (live, fs) = lc
            .prepare_virtiofs_mounts("ch-instance-empty", "default", &[])
            .await
            .unwrap();
        assert!(live.is_empty());
        assert!(fs.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn prepare_mounts_bind() {
        if skip_if_no_virtiofsd("prepare_mounts_bind") {
            return;
        }
        let (lc, dir) = lifecycle_with_socket_dir();
        let bind_src = dir.join("bind-src");
        std::fs::create_dir_all(&bind_src).unwrap();
        std::fs::write(bind_src.join("payload"), b"x").unwrap();

        let mounts = vec![ResolvedMount::Bind {
            source: bind_src.to_string_lossy().into_owned(),
            destination: "/data".to_string(),
            read_only: false,
        }];

        let (live, fs) = lc
            .prepare_virtiofs_mounts("ch-instance-bind", "default", &mounts)
            .await
            .expect("bind prepare should succeed");

        assert_eq!(live.len(), 1);
        assert_eq!(fs.len(), 1);
        assert_eq!(fs[0].tag, "bind-0");
        assert_eq!(live[0].tag, "bind-0");
        assert_eq!(live[0].destination, "/data");
        assert!(!live[0].read_only);
        // Drop the live mount so the daemon dies before scratch cleanup.
        drop(live);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn prepare_mounts_named_creates_persistent_dir() {
        if skip_if_no_virtiofsd("prepare_mounts_named_creates_persistent_dir") {
            return;
        }
        let (lc, dir) = lifecycle_with_socket_dir();

        let mounts = vec![ResolvedMount::Named {
            name: "pgdata".to_string(),
            destination: "/var/lib/postgresql/data".to_string(),
            read_only: false,
            driver: "local".to_string(),
        }];

        let (live, fs) = lc
            .prepare_virtiofs_mounts("ch-instance-vol", "team-a", &mounts)
            .await
            .expect("named prepare should succeed");

        assert_eq!(fs[0].tag, "vol-0");
        // The named volume directory should sit under socket_dir/volumes/<ns>/<name>.
        let expected = dir.join("volumes").join("team-a").join("pgdata");
        assert!(
            expected.is_dir(),
            "named volume dir should exist at {:?}",
            expected
        );

        drop(live);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn prepare_mounts_content_writes_file_in_share_dir() {
        if skip_if_no_virtiofsd("prepare_mounts_content_writes_file_in_share_dir") {
            return;
        }
        let (lc, dir) = lifecycle_with_socket_dir();

        let mounts = vec![ResolvedMount::Content {
            content: "server { listen 80; }".to_string(),
            destination: "/etc/nginx/nginx.conf".to_string(),
        }];

        let instance_id = "ch-instance-content";
        let (live, fs) = lc
            .prepare_virtiofs_mounts(instance_id, "default", &mounts)
            .await
            .expect("content prepare should succeed");

        assert_eq!(fs[0].tag, "cfg-0");
        // The destination's parent is the *guest* mountpoint; on the host,
        // the file lives inside the per-instance share staging dir.
        let staged = lc
            .instance_share_dir(instance_id)
            .join("cfg-0")
            .join("nginx.conf");
        assert!(
            staged.is_file(),
            "config file should be staged at {:?}",
            staged
        );
        let body = std::fs::read_to_string(&staged).unwrap();
        assert_eq!(body, "server { listen 80; }");
        // Guest sees the *parent* directory as the mount point so that the
        // file lands at the user-supplied destination.
        assert_eq!(live[0].destination, "/etc/nginx");
        assert!(live[0].read_only);

        drop(live);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn prepare_mounts_three_kinds_emit_distinct_tags() {
        if skip_if_no_virtiofsd("prepare_mounts_three_kinds_emit_distinct_tags") {
            return;
        }
        let (lc, dir) = lifecycle_with_socket_dir();

        let bind_src = dir.join("bind-src");
        std::fs::create_dir_all(&bind_src).unwrap();

        let mounts = vec![
            ResolvedMount::Bind {
                source: bind_src.to_string_lossy().into_owned(),
                destination: "/host-data".to_string(),
                read_only: false,
            },
            ResolvedMount::Named {
                name: "cache".to_string(),
                destination: "/cache".to_string(),
                read_only: false,
                driver: "local".to_string(),
            },
            ResolvedMount::Content {
                content: "hello=world".to_string(),
                destination: "/etc/conf/app.env".to_string(),
            },
        ];

        let (live, fs) = lc
            .prepare_virtiofs_mounts("ch-instance-mix", "default", &mounts)
            .await
            .expect("mixed prepare should succeed");

        assert_eq!(fs.len(), 3);
        let tags: Vec<&str> = fs.iter().map(|c| c.tag.as_str()).collect();
        assert_eq!(tags, vec!["bind-0", "vol-1", "cfg-2"]);

        drop(live);
        std::fs::remove_dir_all(&dir).ok();
    }
}
