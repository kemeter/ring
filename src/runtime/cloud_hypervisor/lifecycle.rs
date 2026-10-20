use super::client::{
    CloudHypervisorClient, ConsoleConfig, CpuConfig, DiskConfig, FsConfig, MemoryConfig, NetConfig,
    PayloadConfig, VmConfig,
};
use crate::config::config::get_config_dir;
use crate::config::server::CloudHypervisorConfig;
use crate::hypervisor::error::RuntimeError;
use crate::hypervisor::host_net::{InstanceNet, cid_for_instance};
use crate::hypervisor::instance_driver::{InstanceDriver, Observation};
use crate::hypervisor::lifecycle_trait::{Log, RuntimeLifecycle, classify_log, extract_date};
use crate::hypervisor::port_forwarder::{self, PortForwarder};
use crate::hypervisor::virtiofs::{self, VirtiofsMount};
use crate::hypervisor::vm_exits::{self, VmExits, Watched};
use crate::models::deployments::Deployment;
use crate::models::health_check::HealthCheckStatus;
use crate::models::restart_state::{LOGS_TAIL_LINES, Termination, logs_tail};
use crate::models::volume::ResolvedMount;
use crate::runtime::docker::tiny_id;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures::stream::{self, StreamExt};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use tokio::io::AsyncReadExt;
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
    /// Size threshold (bytes) past which the rotation task shifts
    /// `<id>.console.log` to `.1`, `.1` to `.2`, etc. 0 disables rotation.
    pub max_console_log_bytes: u64,
    /// How many rotated backups to keep alongside the live console log.
    pub max_console_log_backups: u32,
}

impl Default for CloudHypervisorRuntimeConfig {
    fn default() -> Self {
        let base_dir = get_config_dir();
        Self {
            binary_path: "cloud-hypervisor".to_string(),
            firmware_path: format!("{}/cloud-hypervisor/vmlinux", base_dir),
            socket_dir: format!("{}/cloud-hypervisor/sockets", base_dir),
            seccomp: None,
            max_console_log_bytes: 10 * 1024 * 1024,
            max_console_log_backups: 3,
        }
    }
}

impl CloudHypervisorRuntimeConfig {
    /// Whether the cloud-hypervisor binary is actually present, so the runtime
    /// can be registered only when it can run. Mirrors the Docker `ping()` gate:
    /// no point advertising a runtime that fails on the first deployment.
    ///
    /// An absolute/relative path must exist on disk; a bare name (the default,
    /// `cloud-hypervisor`) is looked up across `PATH`.
    pub(crate) fn is_available(&self) -> bool {
        let binary = &self.binary_path;
        if binary.contains('/') {
            return std::path::Path::new(binary).exists();
        }
        std::env::var_os("PATH")
            .map(|paths| std::env::split_paths(&paths).any(|dir| dir.join(binary).exists()))
            .unwrap_or(false)
    }

    /// Merge a user-facing config section with the defaults. Any field left
    /// unset in `user` falls back to `CloudHypervisorRuntimeConfig::default`.
    pub(crate) fn from_user_config(user: &CloudHypervisorConfig) -> Self {
        let defaults = Self::default();
        Self {
            binary_path: user.binary_path.clone().unwrap_or(defaults.binary_path),
            firmware_path: user.firmware_path.clone().unwrap_or(defaults.firmware_path),
            socket_dir: user.socket_dir.clone().unwrap_or(defaults.socket_dir),
            seccomp: user.seccomp.clone(),
            max_console_log_bytes: user
                .max_console_log_bytes
                .unwrap_or(defaults.max_console_log_bytes),
            max_console_log_backups: user
                .max_console_log_backups
                .unwrap_or(defaults.max_console_log_backups),
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
    /// PID + memory limit (bytes) per instance. PID is captured at spawn so
    /// stats lookups can read /proc/<pid>/* without shelling out to pgrep;
    /// the memory limit is what we passed to CH at boot so stats can report
    /// `usage_percent` without a second round-trip to the CH API socket.
    /// Removed on stop. Absence means the VM is gone (or was never tracked
    /// by this process — e.g. inherited across a ring-server restart).
    pids: Mutex<HashMap<String, InstanceProcessInfo>>,
    /// How each cloud-hypervisor process this server spawned exited.
    exits: VmExits,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct InstanceProcessInfo {
    pub pid: u32,
    pub memory_limit_bytes: u64,
}

impl CloudHypervisorLifecycle {
    pub fn new(config: CloudHypervisorRuntimeConfig) -> Self {
        Self {
            config,
            virtiofs_mounts: Mutex::new(HashMap::new()),
            port_forwarders: Mutex::new(HashMap::new()),
            pids: Mutex::new(HashMap::new()),
            exits: VmExits::default(),
        }
    }

    /// Spawn a background task that walks the socket directory every 60s and
    /// rotates any `*.console.log` past the configured size threshold. Returns
    /// the handle so the caller (typically `ring-server`) can abort it on
    /// shutdown. No-op (returns a stub task) when rotation is disabled.
    pub fn spawn_console_log_rotator(&self) -> tokio::task::JoinHandle<()> {
        let dir = std::path::PathBuf::from(&self.config.socket_dir);
        let max_bytes = self.config.max_console_log_bytes;
        let max_backups = self.config.max_console_log_backups;
        tokio::spawn(async move {
            if max_bytes == 0 {
                tracing::info!("CH console log rotation disabled (max_console_log_bytes = 0)");
                return;
            }
            tracing::info!(
                "CH console log rotator armed: dir={:?} max_bytes={} max_backups={}",
                dir,
                max_bytes,
                max_backups
            );
            let mut ticker = tokio::time::interval(tokio::time::Duration::from_secs(60));
            // Skip the initial tick so we don't run a sweep before the
            // socket_dir has been populated.
            ticker.tick().await;
            loop {
                ticker.tick().await;
                tracing::debug!("CH console log rotator: sweeping {:?}", dir);
                crate::hypervisor::console_logs::rotate_all_in_dir(&dir, max_bytes, max_backups)
                    .await;
            }
        })
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

    fn deployment_prefix(deployment_id: &str) -> String {
        format!("ch-{}-", &deployment_id[..8.min(deployment_id.len())])
    }

    /// The file name of the cloud-hypervisor binary, as a process started from
    /// it shows in its command line.
    fn binary_name(&self) -> &str {
        Path::new(&self.config.binary_path)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(&self.config.binary_path)
    }

    fn process_info(&self, instance_id: &str) -> Option<InstanceProcessInfo> {
        self.pids.lock().ok()?.get(instance_id).copied()
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
            if let Ok(info) = client.info().await
                && accepted_states.contains(&info.state.as_str())
            {
                instances.push(instance_id);
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

            // Only a writable persistent (Named) volume holds data the operator
            // expects to survive a crash, so only it gets the synchronous
            // `--cache never` policy. Binds and rendered configs keep the
            // faster default cache.
            let (tag, source, destination, read_only, durable) = match m {
                ResolvedMount::Bind {
                    source,
                    destination,
                    read_only,
                } => (
                    format!("bind-{}", idx),
                    PathBuf::from(source),
                    destination.clone(),
                    *read_only,
                    false,
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
                    (
                        format!("vol-{}", idx),
                        dir,
                        destination.clone(),
                        *read_only,
                        !*read_only,
                    )
                }
                ResolvedMount::Content {
                    content,
                    destination,
                    ..
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
                    (format!("cfg-{}", idx), cfg_dir, parent, true, false)
                }
            };

            let mount = virtiofs::spawn_virtiofsd(
                &virtiofsd_path,
                &source,
                &socket_path,
                &tag,
                &destination,
                read_only,
                durable,
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
            .map_err(RuntimeError::Io)?;

        // Permanent: missing VM image. The operator must fix the deployment.
        let base_image = PathBuf::from(&deployment.image);
        if !base_image.exists() {
            return Err(RuntimeError::ImageNotFound(deployment.image.clone()));
        }

        // Pre-check every published port before doing any expensive work
        // (image copy, virtiofsd spawn, VM boot). Mirrors docker compose's
        // "port is already allocated" rejection: if the host can't bind the
        // port now, the VM is doomed to be unreachable on it. Failing here
        // means the scheduler increments restart_count and eventually
        // surfaces a CrashLoopBackOff with a clear PortAllocationFailed
        // event in the deployment history.
        for p in &deployment.ports {
            let host_ip = p
                .host_ip
                .as_deref()
                .unwrap_or(port_forwarder::DEFAULT_HOST_IP);
            if !port_forwarder::host_port_available(host_ip, p.published) {
                return Err(RuntimeError::PortAlreadyInUse(p.published));
            }
        }

        // Admission control before image copy / virtiofsd / VM boot. A CH VM
        // reserves its whole memory at boot, so an over-ask fails the spawn with
        // an opaque "Cannot allocate memory". Catch it here with a clear
        // need/have message instead.
        crate::hypervisor::resources::check_host_memory(deployment)?;

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
                .map_err(RuntimeError::Io)?;

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

        if let Some(pid) = child.id()
            && let Ok(mut map) = self.pids.lock()
        {
            map.insert(
                instance_id.to_string(),
                InstanceProcessInfo {
                    pid,
                    memory_limit_bytes: (memory_mb as u64).saturating_mul(1024 * 1024),
                },
            );
        }

        let stderr = child.stderr.take();

        // Monitor the child process in the background, and record how it ends:
        // that is how a VM whose process is gone gets its exit code.
        let child_instance_id = instance_id.to_string();
        let exits = self.exits.clone();
        exits.watch(instance_id);
        tokio::spawn(async move {
            let stderr_task = stderr.map(|mut s| {
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let _ = s.read_to_end(&mut buf).await;
                    String::from_utf8_lossy(&buf).into_owned()
                })
            });

            let waited = child.wait().await;
            match &waited {
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
            exits.record(&child_instance_id, waited);
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

        let guest_mounts: Vec<crate::hypervisor::cloud_init::GuestMount> = live_mounts
            .iter()
            .map(|m| {
                crate::hypervisor::cloud_init::GuestMount::virtiofs(
                    m.tag.clone(),
                    m.destination.clone(),
                    m.read_only,
                )
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

        // Attach a vhost-vsock device whenever the deployment declares a
        // `command` health check — that's the only consumer today. Adding a
        // vsock for every CH VM would mean an extra device per VM for no
        // benefit; gate it on demand instead.
        //
        // Known limitation: `needs_vsock` is evaluated only at boot, and CH has
        // no hot-attach path for vhost-vsock. A `command` check added to an
        // already-running deployment therefore cannot reach its VM until that
        // VM restarts. Whether it ever does is up to the check's `on_failure`:
        // `restart` heals itself after the failure threshold, but `alert` and
        // `stop` never re-run this path, so the check stays red until the
        // operator restarts the deployment. `execute_command_probe` detects
        // this case (no host-side vsock socket) and says so, rather than
        // blaming a missing `ring-agent` in the guest.
        let needs_vsock = deployment
            .health_checks
            .iter()
            .any(|hc| matches!(hc, crate::models::health_check::HealthCheck::Command { .. }));
        let vsock_cid = if needs_vsock {
            Some(cid_for_instance(instance_id))
        } else {
            None
        };
        let vsock_socket_path = vsock_cid
            .map(|_| PathBuf::from(&self.config.socket_dir).join(format!("{}.vsock", instance_id)));
        let guest_net = net_alloc
            .as_ref()
            .map(|n| crate::hypervisor::cloud_init::GuestNet {
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
            let iso_path = crate::hypervisor::cloud_init::build_cidata_iso(
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
            Some(n) => {
                // The tap CH is about to create lets the guest reach the host;
                // outbound NAT is what lets it reach the Internet (apt, git
                // clone, …). Same call Firecracker makes after creating its own
                // tap: idempotent, scoped to the whole guest supernet, so it is
                // a no-op after the first VM on this host. Without it a CH guest
                // resolved nothing, while an equivalent Firecracker guest did.
                crate::hypervisor::host_nat::ensure_outbound_nat();
                NetConfig {
                    tap: Some(n.tap_name.clone()),
                    ip: Some(n.host_ip.clone()),
                    mask: Some(n.netmask.clone()),
                    mac: Some(n.mac.clone()),
                }
            }
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
            vsock: match (vsock_cid, vsock_socket_path.as_ref()) {
                (Some(cid), Some(path)) => Some(super::client::VsockConfig {
                    cid,
                    socket: Self::path_str(path)?.to_string(),
                }),
                _ => None,
            },
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
        if !live_mounts.is_empty()
            && let Ok(mut map) = self.virtiofs_mounts.lock()
        {
            map.insert(instance_id.to_string(), live_mounts);
        }

        // Spawn one socat per declared port now that the guest IP is reachable
        // (cloud-init has had time to bring eth0 up). The host-port
        // availability was pre-checked before VM boot, but a race with an
        // unrelated process binding the port in the meantime is still
        // possible — in that case we tear the VM down rather than leave it
        // running with a black-hole port. `forwarders` is a local owned
        // Vec; on early return its Drop kills any socat we already spawned.
        if let Some(net) = &net_alloc {
            let mut forwarders = Vec::with_capacity(deployment.ports.len());
            for p in &deployment.ports {
                match port_forwarder::spawn_forwarder(
                    &net.guest_ip,
                    p.published,
                    p.target,
                    p.host_ip.as_deref(),
                    p.protocol,
                )
                .await
                {
                    Ok(fw) => forwarders.push(fw),
                    Err(e) => {
                        self.stop_vm(instance_id).await;
                        return Err(e);
                    }
                }
            }
            if !forwarders.is_empty()
                && let Ok(mut map) = self.port_forwarders.lock()
            {
                map.insert(instance_id.to_string(), forwarders);
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

        if socket.exists() {
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
        } else {
            debug!(
                "Socket {} absent (VM already gone); cleaning artifacts only",
                socket_str
            );
        }

        self.cleanup_instance_artifacts(instance_id).await;

        info!("Cloud Hypervisor VM {} stopped", instance_id);
        true
    }

    /// Remove every on-disk artifact left behind for a deployment that has
    /// no live VM (per-instance disks, console logs, share dirs, etc.).
    /// Used when CH exited on its own and we have no specific instance_id
    /// to target — typical for `kind: job` after the guest powered off.
    /// The deployment-id prefix narrows the scan so sibling deployments are
    /// not touched.
    async fn cleanup_orphaned_artifacts(&self, deployment_id: &str) {
        let prefix = Self::deployment_prefix(deployment_id);
        let mut entries = match tokio::fs::read_dir(&self.config.socket_dir).await {
            Ok(e) => e,
            Err(_) => return,
        };
        let mut seen_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
        while let Ok(Some(entry)) = entries.next_entry().await {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with(&prefix) {
                continue;
            }
            // Derive the instance_id by stripping the first suffix
            // (`.raw`, `.console.log`, `.shares`, `.vsock`, `.sock`, etc.).
            // Rotated console logs end in `.console.log.<N>` — strip the
            // numeric tail before matching.
            let stripped_rotation = match name.rfind('.') {
                Some(idx) if name[idx + 1..].chars().all(|c| c.is_ascii_digit()) => {
                    let head = &name[..idx];
                    if head.ends_with(".console.log") {
                        head
                    } else {
                        name.as_str()
                    }
                }
                _ => name.as_str(),
            };
            for suffix in [
                ".sock",
                ".raw",
                ".console.log",
                ".cidata.iso",
                ".vsock",
                ".shares",
            ] {
                if let Some(id) = stripped_rotation.strip_suffix(suffix) {
                    seen_ids.insert(id.to_string());
                    break;
                }
            }
        }
        for instance_id in seen_ids {
            self.cleanup_instance_artifacts(&instance_id).await;
        }
    }

    /// Remove the on-disk and in-memory artifacts associated with a VM
    /// instance. Idempotent and missing-file-tolerant — runs as part of
    /// `stop_vm` (after the CH API shutdown) and on its own when CH has
    /// already exited (e.g. `kind: job` guest powered off and took the
    /// process down with it).
    async fn cleanup_instance_artifacts(&self, instance_id: &str) {
        let instance_image = self.instance_image_path(instance_id);
        if let Err(e) = tokio::fs::remove_file(&instance_image).await {
            debug!(
                "Failed to remove instance image {:?}: {}",
                instance_image, e
            );
        }

        let cidata_iso = self.cidata_iso_path(instance_id);
        if let Err(e) = tokio::fs::remove_file(&cidata_iso).await {
            debug!("Failed to remove cidata ISO {:?}: {}", cidata_iso, e);
        }

        let console_log = self.console_log_path(instance_id);
        if let Err(e) = tokio::fs::remove_file(&console_log).await {
            debug!("Failed to remove console log {:?}: {}", console_log, e);
        }
        // Sweep any rotated backups (`<id>.console.log.1`, `.2`, ...). The
        // upper bound matches the largest sane `max_console_log_backups`; any
        // missing index is silently skipped.
        for idx in 1u32..=1000 {
            let backup = {
                let mut s = console_log.as_os_str().to_os_string();
                s.push(format!(".{}", idx));
                PathBuf::from(s)
            };
            if !backup.exists() {
                break;
            }
            if let Err(e) = tokio::fs::remove_file(&backup).await {
                debug!("Failed to remove rotated console log {:?}: {}", backup, e);
            }
        }

        let vsock_path =
            PathBuf::from(&self.config.socket_dir).join(format!("{}.vsock", instance_id));
        if let Err(e) = tokio::fs::remove_file(&vsock_path).await {
            debug!("Failed to remove vsock socket {:?}: {}", vsock_path, e);
        }

        if let Ok(mut map) = self.virtiofs_mounts.lock() {
            let _ = map.remove(instance_id);
        }
        if let Ok(mut map) = self.port_forwarders.lock() {
            let _ = map.remove(instance_id);
        }
        if let Ok(mut map) = self.pids.lock() {
            let _ = map.remove(instance_id);
        }

        let share_dir = self.instance_share_dir(instance_id);
        if let Err(e) = tokio::fs::remove_dir_all(&share_dir).await {
            debug!("Failed to remove share dir {:?}: {}", share_dir, e);
        }
    }

    /// Instance ids of a deployment, read from disk: every instance keeps its
    /// disk image until it is removed, whether its VM still runs or not.
    async fn instance_ids(&self, deployment_id: &str) -> Vec<String> {
        let prefix = Self::deployment_prefix(deployment_id);
        let mut ids = Vec::new();
        let Ok(mut entries) = tokio::fs::read_dir(&self.config.socket_dir).await else {
            return ids;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with(&prefix) {
                continue;
            }
            if let Some(id) = name
                .strip_suffix(".raw")
                .or_else(|| name.strip_suffix(".sock"))
                && !ids.iter().any(|known| known == id)
            {
                ids.push(id.to_string());
            }
        }
        ids
    }

    /// Whether the VM of an instance still runs, and how it ended otherwise.
    async fn instance_state(&self, instance_id: &str) -> VmState {
        let watched = self.exits.get(instance_id);
        if let Some(Watched::Exited(exit)) = watched {
            return VmState::Ended {
                exit_code: exit.exit_code,
                finished_at: exit.finished_at,
            };
        }
        let socket = self.socket_path(instance_id);
        let info = match socket.to_str() {
            Some(path) if socket.exists() => CloudHypervisorClient::new(path).info().await.ok(),
            _ => None,
        };
        match info {
            // The guest powered off and the VMM stayed up. CH does not expose
            // the guest's exit code: a clean power-off is a success.
            Some(info) if info.state == "Shutdown" => VmState::Ended {
                exit_code: Some(0),
                finished_at: Utc::now(),
            },
            Some(_) => VmState::Alive,
            // A process this server spawned and saw no exit of still runs: its
            // API is only slow to answer.
            None if watched.is_some() => VmState::Alive,
            // Inherited from an earlier server: alive while a cloud-hypervisor
            // process holds its socket, whatever its API says.
            None if socket.to_str().is_some_and(|path| {
                socket.exists() && vm_exits::vmm_holds_socket(self.binary_name(), path)
            }) =>
            {
                VmState::Alive
            }
            // Inherited and gone, so how it ended was not seen.
            // cloud-hypervisor removes its socket when it exits cleanly; a
            // crash leaves the socket behind.
            None => VmState::Ended {
                exit_code: (!socket.exists()).then_some(0),
                finished_at: Utc::now(),
            },
        }
    }
}

enum VmState {
    Alive,
    Ended {
        exit_code: Option<i64>,
        finished_at: DateTime<Utc>,
    },
}

#[async_trait]
impl InstanceDriver for CloudHypervisorLifecycle {
    async fn observe(&self, deployment: &Deployment) -> Result<Observation, RuntimeError> {
        let ids = self.instance_ids(&deployment.id).await;
        self.exits
            .retain_known(&Self::deployment_prefix(&deployment.id), &ids);

        let mut observation = Observation::default();
        for instance_id in ids {
            match self.instance_state(&instance_id).await {
                VmState::Alive => observation.running.push(instance_id),
                VmState::Ended {
                    exit_code,
                    finished_at,
                } => {
                    let path = self.console_log_path(&instance_id);
                    let tail = LOGS_TAIL_LINES.to_string();
                    let lines =
                        crate::hypervisor::console_logs::read_lines(&path, Some(&tail), None).await;
                    observation.terminated.push(Termination {
                        instance_id,
                        exit_code,
                        finished_at,
                        started: true,
                        logs_tail: logs_tail(&lines),
                    });
                }
            }
        }
        observation.terminated.sort_by_key(|t| t.finished_at);
        Ok(observation)
    }

    async fn start_instance(
        &self,
        deployment: &mut Deployment,
        resolved_mounts: &[ResolvedMount],
    ) -> Result<(), RuntimeError> {
        let instance_id = format!(
            "ch-{}-{}",
            &deployment.id[..8.min(deployment.id.len())],
            tiny_id()
        );
        match self
            .start_vm_process(&instance_id, deployment, resolved_mounts)
            .await
        {
            Ok(()) => {
                deployment.instances.push(instance_id);
                Ok(())
            }
            Err(e) => {
                // Whatever the failed start left behind (disk copy, socket)
                // would otherwise be reported as an instance that ended.
                self.stop_vm(&instance_id).await;
                Err(e)
            }
        }
    }

    /// `stop_vm` removes the instance's disk image before returning, so a
    /// stopped instance is never reported as a termination.
    async fn stop_instance(&self, instance_id: &str) -> bool {
        self.stop_vm(instance_id).await
    }

    async fn discard_instance(&self, instance_id: &str) {
        self.stop_vm(instance_id).await;
    }

    /// A finished job's VM goes like any other: its disk copy is too large to
    /// keep for its logs, whose end the scheduler already recorded.
    async fn retire_instance(&self, instance_id: &str) {
        self.stop_vm(instance_id).await;
    }

    async fn teardown(&self, deployment: &mut Deployment) -> Result<(), RuntimeError> {
        for instance_id in self.instance_ids(&deployment.id).await {
            if !self.stop_vm(&instance_id).await {
                warn!("Failed to stop VM {} during deletion", instance_id);
            }
        }
        self.cleanup_orphaned_artifacts(&deployment.id).await;
        // What survived a failed stop keeps the deployment for a later pass.
        deployment.instances = self.instance_ids(&deployment.id).await;
        Ok(())
    }
}

/// Translate the runtime-agnostic `status` filter of
/// [`RuntimeLifecycle::list_instances`] into the Cloud Hypervisor VM states that
/// satisfy it. `None` means "no filter" (the `all` case).
///
/// The filter vocabulary is Docker's, since that is what the trait's callers
/// speak: `all` accepts everything, `active` means running-or-coming-up, and
/// anything else is an exact state match.
///
/// `active` maps to the same trio this runtime already treats as "alive or
/// about to be" when scaling and when running a job — the counterpart of
/// Docker's running/restarting. It is deliberately wider than Docker's
/// `active`, which excludes `created`: a CH VM sits in `Created` for a moment
/// between spawn and boot, and dropping it there would make a freshly-started
/// replica briefly invisible.
///
/// This used to be hardcoded to `["Running"]` regardless of the argument, so a
/// caller asking for `all` (or for a stopped instance) silently got only the
/// running ones.
fn ch_states_for_status(status: &str) -> Option<Vec<&'static str>> {
    match status {
        "all" => None,
        "active" => Some(vec!["Running", "Created", "Booting"]),
        "running" => Some(vec!["Running"]),
        "exited" | "stopped" => Some(vec!["Shutdown"]),
        // An explicit CH state name, passed through as-is.
        "Created" => Some(vec!["Created"]),
        "Booting" => Some(vec!["Booting"]),
        "Running" => Some(vec!["Running"]),
        "Shutdown" => Some(vec!["Shutdown"]),
        "Paused" => Some(vec!["Paused"]),
        "BreakPoint" => Some(vec!["BreakPoint"]),
        // Unknown filter: match nothing rather than silently returning
        // everything, which would misreport a deployment as fully up.
        _ => Some(Vec::new()),
    }
}

fn parse_resources(deployment: &Deployment) -> (u32, u32) {
    let mut vcpus = 1u32;
    let mut memory_mb = 256u32;

    if let Some(ref resources) = deployment.resources
        && let Some(ref limits) = resources.limits
    {
        if let Some(ref cpu) = limits.cpu
            && let Ok(nano) = crate::models::deployments::parse_cpu_string(cpu)
        {
            vcpus = std::cmp::max(1, (nano / 1_000_000_000) as u32);
        }
        if let Some(ref mem) = limits.memory
            && let Ok(bytes) = crate::models::deployments::parse_memory_string(mem)
        {
            memory_mb = std::cmp::max(128, (bytes / (1024 * 1024)) as u32);
        }
    }

    (vcpus, memory_mb)
}

#[async_trait]
impl RuntimeLifecycle for CloudHypervisorLifecycle {
    async fn list_instances(&self, deployment_id: String, status: &str) -> Vec<String> {
        match ch_states_for_status(status) {
            Some(states) => self.scan_instances(&deployment_id, &states).await,
            // "all" — no state filter at all, which also skips one API round
            // trip per instance.
            None => self.scan_instances(&deployment_id, &[]).await,
        }
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
    ) -> Vec<Log> {
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
            let lines = crate::hypervisor::console_logs::read_lines(&path, tail, since).await;
            for message in lines {
                logs.push(Log {
                    instance: instance_id.clone(),
                    level: classify_log(&message),
                    timestamp: extract_date(&message),
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
            let raw = crate::hypervisor::console_logs::stream_lines(
                path,
                tail.map(|s| s.to_string()),
                since,
            )
            .await;

            let mapped = raw.map(move |line| {
                let log = Log {
                    instance: owned_id.clone(),
                    level: classify_log(&line),
                    timestamp: extract_date(&line),
                    message: line,
                };
                let json = serde_json::to_string(&log).unwrap_or_default();
                Ok(axum::response::sse::Event::default().data(json))
            });

            streams.push(Box::pin(mapped));
        }

        Box::pin(stream::select_all(streams))
    }

    /// Resolve the guest IP for `instance_id` from the deterministic /30
    /// allocation. The mapping is a pure function of the instance id (see
    /// `InstanceNet::for_instance`), so we don't need any persistent state
    /// — every probe recomputes the same address the VM was booted with.
    async fn instance_address(&self, instance_id: &str) -> Option<std::net::IpAddr> {
        InstanceNet::for_instance(instance_id).guest_ip.parse().ok()
    }

    /// Execute the probe via vsock against `ring-agent` running in the guest.
    /// The CID was assigned deterministically at boot from the instance id;
    /// recompute it here so we don't need to thread state through the probe
    /// path. The agent is responsible for shell-parsing the command — it
    /// receives the string verbatim and lets the guest's shell deal with it.
    async fn execute_command_probe(
        &self,
        instance_id: &str,
        command: &str,
    ) -> (
        crate::models::health_check::HealthCheckStatus,
        Option<String>,
    ) {
        let cid = cid_for_instance(instance_id);
        let argv = vec!["/bin/sh".to_string(), "-c".to_string(), command.to_string()];
        let timeout = std::time::Duration::from_secs(30);

        match crate::hypervisor::vsock_client::exec(cid, &argv, &[], timeout).await {
            Ok(resp) if resp.timed_out => (
                HealthCheckStatus::Timeout,
                Some(format!("command timed out: {}", command)),
            ),
            Ok(resp) if resp.exit_code == 0 => (HealthCheckStatus::Success, None),
            Ok(resp) => (
                HealthCheckStatus::Failed,
                Some(format!(
                    "exit code {}: {}",
                    resp.exit_code,
                    resp.stderr.trim()
                )),
            ),
            // A connect failure has two very different causes, and blaming the
            // guest image for both sends the operator hunting in the wrong
            // place. The host-side socket only exists when the VM was booted
            // with a vsock device, so its absence pinpoints the other cause.
            Err(crate::hypervisor::vsock_client::VsockError::Connect { cid, source }) => {
                let vsock_path =
                    PathBuf::from(&self.config.socket_dir).join(format!("{}.vsock", instance_id));
                (
                    HealthCheckStatus::Failed,
                    Some(crate::hypervisor::vsock_client::connect_failure_message(
                        "cloud-hypervisor",
                        cid,
                        &source.to_string(),
                        vsock_path.exists(),
                    )),
                )
            }
            Err(e) => (
                HealthCheckStatus::Failed,
                Some(format!("vsock probe failed: {}", e)),
            ),
        }
    }

    /// Fan out `read_instance_stats` over each running VM of the deployment.
    /// CPU% requires two samples spaced apart; we sleep a short interval
    /// between reads, so this call blocks for ~`SAMPLE_INTERVAL_MS`. Docker's
    /// `stats` API has the same shape (one-shot still costs a sampling
    /// round-trip), so this is consistent with what the operator already
    /// experiences on the Docker runtime.
    async fn get_instance_stats(
        &self,
        deployment_id: &str,
    ) -> Vec<crate::api::dto::stats::InstanceStatsOutput> {
        let instances = self.scan_instances(deployment_id, &["Running"]).await;
        let mut out = Vec::with_capacity(instances.len());
        for instance_id in instances {
            if let Some(stats) = self.read_instance_stats(&instance_id).await {
                out.push(stats);
            }
        }
        out
    }
}

/// Sampling window for CPU%: long enough for ticks to accumulate on an idle
/// VM, short enough that an HTTP `metrics` call doesn't feel laggy. Docker's
/// stream emits a frame about every second, so we mirror that.
const CPU_SAMPLE_INTERVAL_MS: u64 = 500;

impl CloudHypervisorLifecycle {
    /// Sample CPU twice with a short delay, read RSS once, and assemble the
    /// `InstanceStatsOutput`. Returns `None` if the process tracking entry
    /// is gone (VM stopped, or this server didn't spawn the VM and so has no
    /// PID for it — typical after a ring-server restart).
    async fn read_instance_stats(
        &self,
        instance_id: &str,
    ) -> Option<crate::api::dto::stats::InstanceStatsOutput> {
        let info = self.process_info(instance_id)?;

        let prev = crate::hypervisor::stats::read_cpu_sample(info.pid).await?;
        tokio::time::sleep(tokio::time::Duration::from_millis(CPU_SAMPLE_INTERVAL_MS)).await;
        let curr = crate::hypervisor::stats::read_cpu_sample(info.pid).await?;

        let interval_secs = CPU_SAMPLE_INTERVAL_MS as f64 / 1000.0;
        // SC_CLK_TCK is fixed at compile time on Linux (typically 100). Reading
        // it via libc would force a sysconf dependency; the constant is stable.
        let cpu_usage_percent =
            crate::hypervisor::stats::compute_cpu_percent(prev, curr, interval_secs, 100.0);

        let rss = crate::hypervisor::stats::read_rss_bytes(info.pid).await;
        let memory = crate::hypervisor::stats::memory_stats(rss, info.memory_limit_bytes);

        let tap_name = InstanceNet::for_instance(instance_id).tap_name;
        let network = crate::hypervisor::stats::network_stats_from_tap(&tap_name).await;
        let disk_io = crate::hypervisor::stats::disk_io_stats(info.pid).await;
        let pids = crate::hypervisor::stats::pid_stats(info.pid).await;

        Some(crate::api::dto::stats::InstanceStatsOutput {
            instance_id: instance_id.to_string(),
            instance_name: instance_id.to_string(),
            cpu_usage_percent,
            memory,
            network,
            disk_io,
            pids,
            restart_count: 0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::deployments::DeploymentStatus;
    use std::os::unix::process::ExitStatusExt;

    fn cfg_with_binary(binary_path: &str) -> CloudHypervisorRuntimeConfig {
        CloudHypervisorRuntimeConfig {
            binary_path: binary_path.to_string(),
            ..CloudHypervisorRuntimeConfig::default()
        }
    }

    #[test]
    fn is_available_true_for_existing_absolute_path() {
        assert!(cfg_with_binary("/bin/sh").is_available());
    }

    #[test]
    fn is_available_false_for_missing_absolute_path() {
        assert!(!cfg_with_binary("/nonexistent/cloud-hypervisor-xyz").is_available());
    }

    #[test]
    fn is_available_resolves_bare_name_via_path() {
        assert!(cfg_with_binary("sh").is_available());
        assert!(!cfg_with_binary("definitely-not-a-real-binary-xyz").is_available());
    }

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
            autoscale: None,
            desired_replicas: None,
            image_digest: None,
            ports: vec![],
            pending_events: vec![],
            parent_id: None,
            network: None,
            restart: None,
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
            autoscale: None,
            desired_replicas: None,
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
            network: None,
            restart: None,
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
            max_console_log_bytes: 10 * 1024 * 1024,
            max_console_log_backups: 3,
        };
        (CloudHypervisorLifecycle::new(cfg), socket_dir)
    }

    fn skip_if_no_virtiofsd(test: &str) -> bool {
        if crate::hypervisor::virtiofs::locate_virtiofsd().is_none() {
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
            live_slot: None,
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

    fn job_deployment(status: DeploymentStatus) -> Deployment {
        Deployment {
            id: "job1234-5678".to_string(),
            created_at: String::new(),
            updated_at: None,
            status,
            restart_count: 0,
            namespace: "default".to_string(),
            name: "job".to_string(),
            image: "/tmp/does-not-exist.img".to_string(),
            config: None,
            runtime: "cloud-hypervisor".to_string(),
            kind: "job".to_string(),
            replicas: 1,
            command: vec![],
            instances: vec![],
            labels: std::collections::HashMap::new(),
            environment: std::collections::HashMap::new(),
            volumes: "[]".to_string(),
            health_checks: vec![],
            resources: None,
            autoscale: None,
            desired_replicas: None,
            image_digest: None,
            ports: vec![],
            pending_events: vec![],
            parent_id: None,
            network: None,
            restart: None,
        }
    }

    fn touch(dir: &Path, name: &str) {
        std::fs::write(dir.join(name), b"").unwrap();
    }

    /// Instances are read from their disk image, or their socket alone, and a
    /// sibling deployment's files are not.
    #[tokio::test]
    async fn instance_ids_come_from_disk_images_and_sockets() {
        let (lc, dir) = lifecycle_with_socket_dir();
        touch(&dir, "ch-job1234--aaaa.raw");
        touch(&dir, "ch-job1234--aaaa.sock");
        touch(&dir, "ch-job1234--aaaa.console.log");
        touch(&dir, "ch-job1234--bbbb.sock");
        touch(&dir, "ch-other00-cccc.raw");
        let mut ids = lc.instance_ids("job1234-5678").await;
        ids.sort();
        assert_eq!(ids, vec!["ch-job1234--aaaa", "ch-job1234--bbbb"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A VM whose process exited is reported as terminated with the exit code
    /// it recorded; one still watched runs. One inherited from an earlier
    /// server ended cleanly when its socket is gone, and crashed when its
    /// socket stayed behind, unanswered.
    #[tokio::test]
    async fn observe_reports_how_each_vm_ended() {
        let (lc, dir) = lifecycle_with_socket_dir();
        touch(&dir, "ch-job1234--done.raw");
        touch(&dir, "ch-job1234--live.raw");
        touch(&dir, "ch-job1234--lost.raw");
        touch(&dir, "ch-job1234--lost.sock");
        touch(&dir, "ch-job1234--gone.raw");
        lc.exits.watch("ch-job1234--done");
        lc.exits.record(
            "ch-job1234--done",
            Ok(std::process::ExitStatus::from_raw(0)),
        );
        lc.exits.watch("ch-job1234--live");

        let observation = lc
            .observe(&job_deployment(DeploymentStatus::Running))
            .await
            .unwrap();

        assert_eq!(observation.running, vec!["ch-job1234--live"]);
        let mut ended: Vec<_> = observation
            .terminated
            .iter()
            .map(|t| (t.instance_id.as_str(), t.exit_code))
            .collect();
        ended.sort();
        assert_eq!(
            ended,
            vec![
                ("ch-job1234--done", Some(0)),
                ("ch-job1234--gone", Some(0)),
                ("ch-job1234--lost", None)
            ]
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A start that fails leaves nothing behind that would read as an
    /// instance that ended.
    #[tokio::test]
    async fn a_failed_start_leaves_no_instance() {
        let (lc, dir) = lifecycle_with_socket_dir();
        let mut deployment = job_deployment(DeploymentStatus::Creating);
        let err = lc.start_instance(&mut deployment, &[]).await.unwrap_err();
        assert!(matches!(err, RuntimeError::FirmwareNotFound(_)));
        assert!(deployment.instances.is_empty());
        assert!(lc.instance_ids(&deployment.id).await.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The status argument used to be ignored — `list_instances` always scanned
    /// for `Running` only, so a caller asking for `all` (or for a stopped
    /// instance) silently got just the running ones.
    #[test]
    fn status_filter_maps_to_ch_states() {
        // `all` means no state filter at all, which also skips one API round
        // trip per instance.
        assert_eq!(ch_states_for_status("all"), None);
        // `active` is running-or-coming-up, mirroring Docker's
        // running/restarting intent.
        assert_eq!(
            ch_states_for_status("active"),
            Some(vec!["Running", "Created", "Booting"])
        );
        assert_eq!(ch_states_for_status("running"), Some(vec!["Running"]));
        assert_eq!(ch_states_for_status("exited"), Some(vec!["Shutdown"]));
        assert_eq!(ch_states_for_status("stopped"), Some(vec!["Shutdown"]));
    }

    /// A raw CH state name passes through, so a caller can ask for exactly one.
    #[test]
    fn explicit_ch_state_names_pass_through() {
        for state in ["Created", "Booting", "Running", "Shutdown"] {
            assert_eq!(ch_states_for_status(state), Some(vec![state]));
        }
    }

    /// An unrecognised filter must match nothing rather than fall back to
    /// everything, which would misreport a deployment as fully up.
    #[test]
    fn unknown_status_filter_matches_nothing() {
        assert_eq!(ch_states_for_status("paused"), Some(Vec::new()));
        assert_eq!(ch_states_for_status(""), Some(Vec::new()));
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
                live_slot: None,
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
