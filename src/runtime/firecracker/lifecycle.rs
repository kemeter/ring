//! Firecracker microVM runtime.
//!
//! Boots `replicas` microVMs from a kernel (config) + a per-deployment rootfs
//! (`deployment.image` on the host), tracks them by their API socket, scales
//! up/down, and tears down. At parity with the Cloud Hypervisor runtime via the
//! shared helpers (`host_net`, `port_forwarder`, `vsock_client`): networking
//! with outbound NAT, per-instance stats, serial-console logs, `command` health
//! checks through the in-guest `ring-agent` over vsock, `kind: job`
//! run-to-completion, and volumes mounted as virtio-block ext4 images.
//!
//! Mirrors `cloud_hypervisor::lifecycle` structure: a `*RuntimeConfig` with
//! defaults + `is_available` + `from_user_config`, a lifecycle struct holding
//! per-instance PIDs, and an instance id of `<deployment_id>-<tiny_id>` whose
//! presence on disk (its `.sock`) is the source of truth for "is it running".

use crate::config::server::FirecrackerConfig;
use crate::hypervisor::classifier::{apply_vm_start_failure, classify_vm_start_error};
use crate::hypervisor::cloud_init::{GuestMount, GuestNet};
use crate::hypervisor::error::RuntimeError;
use crate::hypervisor::host_net::{InstanceNet, cid_for_instance};
use crate::hypervisor::lifecycle_trait::{Log, RuntimeLifecycle, classify_log, extract_date};
use crate::hypervisor::port_forwarder::{self, PortForwarder};
use crate::hypervisor::tap::TapDevice;
use crate::hypervisor::volume_image as vol;
use crate::hypervisor::vsock_client::{self, VsockError};
use crate::models::deployments::{Deployment, DeploymentStatus, MAX_RESTART_COUNT};
use crate::models::health_check::{HealthCheck, HealthCheckStatus};
use crate::models::volume::ResolvedMount;
use crate::runtime::docker::tiny_id;
use crate::runtime::firecracker::client::{
    BootSource, Drive, FirecrackerClient, MachineConfig, NetworkInterface, Vsock,
};
use async_trait::async_trait;
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
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
    /// Maximum size (bytes) for a per-VM console log before rotation. Defaults
    /// to 10 MiB. Set to 0 to disable rotation entirely.
    pub max_console_log_bytes: u64,
    /// How many rotated console log backups to keep. Defaults to 3.
    pub max_console_log_backups: u32,
}

impl Default for FirecrackerRuntimeConfig {
    fn default() -> Self {
        let base_dir = crate::config::config::get_config_dir();
        Self {
            binary_path: "firecracker".to_string(),
            kernel_path: format!("{}/firecracker/vmlinux", base_dir),
            socket_dir: format!("{}/firecracker/sockets", base_dir),
            boot_args: "console=ttyS0 reboot=k panic=1 pci=off".to_string(),
            max_console_log_bytes: 10 * 1024 * 1024,
            max_console_log_backups: 3,
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
            max_console_log_bytes: user
                .max_console_log_bytes
                .unwrap_or(defaults.max_console_log_bytes),
            max_console_log_backups: user
                .max_console_log_backups
                .unwrap_or(defaults.max_console_log_backups),
        }
    }
}

pub struct FirecrackerLifecycle {
    config: FirecrackerRuntimeConfig,
    /// Process info per instance id, captured at spawn so teardown can kill the
    /// right process and stats can report `memory.usage_percent` without a
    /// round-trip to the VMM. Absence means the VM is gone (or was never
    /// tracked by this process — e.g. inherited across a ring-server restart).
    pids: Mutex<HashMap<String, InstanceProcessInfo>>,
    /// Live host tap devices, keyed by instance id. Unlike Cloud Hypervisor,
    /// Firecracker doesn't create the tap itself — Ring owns its whole
    /// lifecycle. Dropping the entry deletes the interface from the host.
    taps: Mutex<HashMap<String, TapDevice>>,
    /// Live socat port-forwarders, keyed by instance id. Dropping the entry
    /// kills the socat process.
    port_forwarders: Mutex<HashMap<String, Vec<PortForwarder>>>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct InstanceProcessInfo {
    /// The `firecracker` process PID — used for teardown and `/proc/<pid>` stats.
    pub pid: u32,
    /// Memory limit (bytes) handed to the microVM at boot, so stats can report
    /// `usage_percent`. 0 when the VM was inherited across a restart (we never
    /// re-read the original limit) — reported as "unlimited", matching Docker.
    pub memory_limit_bytes: u64,
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

    /// Per-instance ephemeral volume image (Bind/Content). One file per volume
    /// index, reaped when the instance stops.
    fn ephemeral_volume_path(&self, instance_id: &str, idx: usize) -> String {
        format!("{}/{}.vol{}.ext4", self.config.socket_dir, instance_id, idx)
    }

    /// Persistent Named-volume image, shared across instances of the same
    /// namespace+name and NOT reaped on stop. Lives under `volumes/<ns>/`.
    fn named_volume_path(&self, namespace: &str, name: &str) -> PathBuf {
        PathBuf::from(&self.config.socket_dir)
            .join("volumes")
            .join(namespace)
            .join(format!("{}.ext4", name))
    }

    /// Build + attach a virtio-block device for each resolved mount, returning
    /// the cloud-init `GuestMount`s the guest needs to mount them. Drives attach
    /// in declaration order right after the root device, so volume N is
    /// `/dev/vd{b,c,...}` (index 0 → `vdb`).
    ///
    /// - **Bind**: a fresh ext4 image seeded from the host source directory.
    /// - **Content**: a fresh ext4 image holding the single rendered file.
    /// - **Named**: a persistent ext4 image created once and reused.
    async fn prepare_volume_drives(
        &self,
        client: &FirecrackerClient,
        instance_id: &str,
        deployment: &Deployment,
        resolved_mounts: &[ResolvedMount],
    ) -> Result<Vec<crate::hypervisor::cloud_init::GuestMount>, String> {
        // /dev/vda is root and cidata takes the free letter after the last
        // volume, so volumes can use vdb..=vdy at most — 24 of them. Past that
        // the device letters would overflow into punctuation ('{', '|', …) and
        // silently corrupt the boot; fail loudly instead.
        const MAX_VOLUMES: usize = 24;
        if resolved_mounts.len() > MAX_VOLUMES {
            return Err(format!(
                "firecracker supports at most {} volumes, got {}",
                MAX_VOLUMES,
                resolved_mounts.len()
            ));
        }

        let mut guest_mounts = Vec::with_capacity(resolved_mounts.len());

        for (idx, m) in resolved_mounts.iter().enumerate() {
            // /dev/vda is root; volumes start at vdb.
            let dev_letter = (b'b' + idx as u8) as char;
            let device = format!("/dev/vd{}", dev_letter);
            let label = format!("ringvol{}", idx);

            let (img_path, destination, read_only) = match m {
                ResolvedMount::Bind {
                    source,
                    destination,
                    read_only,
                } => {
                    let src = Path::new(source);
                    if !src.is_dir() {
                        return Err(format!(
                            "firecracker bind volume source '{}' is not a directory on the host",
                            source
                        ));
                    }
                    let size = vol::sizing_mib_for_bytes(vol::dir_size_bytes(src).await);
                    let img = PathBuf::from(self.ephemeral_volume_path(instance_id, idx));
                    vol::build_ext4_from_dir(&img, src, size, &label)
                        .await
                        .map_err(|e| e.to_string())?;
                    (img, destination.clone(), *read_only)
                }
                ResolvedMount::Content {
                    content,
                    destination,
                } => {
                    // Stage the single rendered file under its basename, build
                    // an ext4 from that dir, and mount it at the destination's
                    // PARENT directory (mirroring the CH semantics where the
                    // file lands at the user-supplied path).
                    let dest_path = Path::new(destination);
                    let filename = dest_path.file_name().ok_or_else(|| {
                        format!(
                            "content volume destination has no filename: {}",
                            destination
                        )
                    })?;
                    let mount_dir = dest_path
                        .parent()
                        .map(|p| p.to_string_lossy().to_string())
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| "/".to_string());

                    let stage = PathBuf::from(self.config.socket_dir.clone())
                        .join(format!("{}.vol{}.stage", instance_id, idx));
                    if stage.exists() {
                        let _ = tokio::fs::remove_dir_all(&stage).await;
                    }
                    tokio::fs::create_dir_all(&stage)
                        .await
                        .map_err(|e| e.to_string())?;
                    tokio::fs::write(stage.join(filename), content.as_bytes())
                        .await
                        .map_err(|e| e.to_string())?;

                    let size = vol::sizing_mib_for_bytes(content.len() as u64);
                    let img = PathBuf::from(self.ephemeral_volume_path(instance_id, idx));
                    let built = vol::build_ext4_from_dir(&img, &stage, size, &label).await;
                    let _ = tokio::fs::remove_dir_all(&stage).await;
                    built.map_err(|e| e.to_string())?;
                    // Content is rendered config — read-only in the guest.
                    (img, mount_dir, true)
                }
                ResolvedMount::Named {
                    name,
                    destination,
                    read_only,
                    ..
                } => {
                    let img = self.named_volume_path(&deployment.namespace, name);
                    if !img.exists() {
                        // 256 MiB default for a fresh persistent volume; the
                        // guest can grow usage up to that.
                        vol::create_empty_ext4(&img, 256, &label)
                            .await
                            .map_err(|e| e.to_string())?;
                    }
                    (img, destination.clone(), *read_only)
                }
            };

            client
                .put_drive(&Drive {
                    drive_id: format!("vol{}", idx),
                    path_on_host: img_path.to_string_lossy().to_string(),
                    is_root_device: false,
                    is_read_only: read_only,
                })
                .await
                .map_err(|e| e.to_string())?;

            guest_mounts.push(GuestMount::block(device, destination, read_only));
        }

        Ok(guest_mounts)
    }

    /// Remove a stopped instance's ephemeral volume images (Bind/Content) and
    /// any leftover staging dirs. Persistent Named volumes live under
    /// `volumes/` and are intentionally left in place.
    ///
    /// Ephemeral indices are sparse — a Named volume occupies an index but has
    /// no `.vol{idx}.ext4` file — so we scan the socket dir for this instance's
    /// `*.vol*.ext4` images and `*.vol*.stage` dirs rather than walking indices
    /// (which would stop at the first gap and leak the rest).
    fn cleanup_ephemeral_volumes(&self, instance_id: &str) {
        let img_prefix = format!("{}.vol", instance_id);
        let entries = match std::fs::read_dir(&self.config.socket_dir) {
            Ok(e) => e,
            Err(_) => return,
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if !name.starts_with(&img_prefix) {
                continue;
            }
            let path = entry.path();
            if name.ends_with(".ext4") {
                let _ = std::fs::remove_file(&path);
            } else if name.ends_with(".stage") {
                // Staging dir orphaned by a crash mid-build; normally removed
                // inline once the image is built.
                let _ = std::fs::remove_dir_all(&path);
            }
        }
    }

    /// Spawn a background task that walks the socket directory every 60s and
    /// rotates any `*.console.log` past the configured size threshold. Returns
    /// the handle so the caller (`ring-server`) can abort it on shutdown.
    ///
    /// Unlike Cloud Hypervisor, Firecracker holds its console fd by inode (it's
    /// the spawned process' stdout), so this uses **copy-truncate** rather than
    /// rename — see [`crate::hypervisor::console_logs::copy_truncate_all_in_dir`].
    /// No-op (logs once) when rotation is disabled.
    pub fn spawn_console_log_rotator(&self) -> tokio::task::JoinHandle<()> {
        let dir = std::path::PathBuf::from(&self.config.socket_dir);
        let max_bytes = self.config.max_console_log_bytes;
        let max_backups = self.config.max_console_log_backups;
        tokio::spawn(async move {
            if max_bytes == 0 {
                tracing::info!(
                    "Firecracker console log rotation disabled (max_console_log_bytes = 0)"
                );
                return;
            }
            tracing::info!(
                "Firecracker console log rotator armed: dir={:?} max_bytes={} max_backups={}",
                dir,
                max_bytes,
                max_backups
            );
            let mut ticker = tokio::time::interval(tokio::time::Duration::from_secs(60));
            // Skip the initial tick so we don't sweep before socket_dir exists.
            ticker.tick().await;
            loop {
                ticker.tick().await;
                tracing::debug!("Firecracker console log rotator: sweeping {:?}", dir);
                crate::hypervisor::console_logs::copy_truncate_all_in_dir(
                    &dir,
                    max_bytes,
                    max_backups,
                )
                .await;
            }
        })
    }

    /// Boot one worker microVM. Returns the new instance id on success.
    async fn start_vm(
        &self,
        deployment: &Deployment,
        resolved_mounts: &[ResolvedMount],
    ) -> Result<String, RuntimeError> {
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

        // Admission control before the rootfs copy and the VM boot. A microVM
        // reserves its whole memory at boot, so an over-ask dies on an opaque
        // allocation failure — and here it would do so only *after* copying a
        // full rootfs image. Same check Cloud Hypervisor already ran; without it
        // Firecracker had no memory gate at all.
        crate::hypervisor::resources::check_host_memory(deployment)?;

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
        //
        // Opened with O_APPEND (not a plain create): Firecracker holds this fd
        // for the VM's whole life, so the rotator can't make it reopen by name
        // the way Cloud Hypervisor does. With O_APPEND every write seeks to EOF
        // first, which makes copy-truncate rotation land new output at offset 0
        // instead of leaving a sparse hole. The instance id is unique per boot
        // (`<deployment>-<tiny_id>`), so there is no stale file to clear.
        let console_log = self.console_log_path(&instance_id);
        let (out, err): (std::process::Stdio, std::process::Stdio) =
            match std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .truncate(false)
                .open(&console_log)
            {
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

        // Every microVM gets a network. A guest is an isolated machine that
        // needs connectivity to be useful at all: outbound to fetch what it runs
        // (the guest init clones its source, pulls dependencies) and inbound to
        // be reachable — independent of whether it publishes a port. Allocate a
        // deterministic /30 and create the host tap. Unlike Cloud Hypervisor,
        // Firecracker does not create the tap — Ring creates it here (held in
        // `tap` so an early return on any later error deletes it via Drop) and
        // hands its name to Firecracker, while cloud-init configures the
        // matching guest IP.
        let net_alloc = Some(InstanceNet::for_instance(&instance_id));
        if let Some(n) = &net_alloc {
            self.reclaim_stale_tap(&n.tap_name, &instance_id);
        }
        let tap = match &net_alloc {
            Some(n) => match TapDevice::create(&n.tap_name, &n.host_ip, n.prefix_len) {
                Ok(t) => {
                    // The tap lets the guest reach the host; outbound NAT lets it
                    // reach the Internet (git clone, composer, …). Idempotent and
                    // global to the guest supernet, so it's a no-op after the
                    // first VM. The operator never touches iptables.
                    crate::hypervisor::host_nat::ensure_outbound_nat();
                    Some(t)
                }
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
                resolved_mounts,
            )
            .await;
        if let Err(e) = boot {
            // `tap` drops here, deleting the interface.
            self.kill_pid(pid).await;
            let _ = std::fs::remove_file(&socket_path);
            let _ = std::fs::remove_file(&rootfs_rw);
            let _ = std::fs::remove_file(self.cidata_path(&instance_id));
            self.cleanup_ephemeral_volumes(&instance_id);
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

        let (_, mem_mib) = parse_resources(deployment);
        let memory_limit_bytes = (mem_mib as u64).saturating_mul(1024 * 1024);
        self.pids.lock().unwrap().insert(
            instance_id.clone(),
            InstanceProcessInfo {
                pid,
                memory_limit_bytes,
            },
        );
        if let Some(t) = tap {
            self.taps.lock().unwrap().insert(instance_id.clone(), t);
        }
        info!("Firecracker microVM {} booted (pid {})", instance_id, pid);
        Ok(instance_id)
    }

    fn cidata_path(&self, instance_id: &str) -> String {
        format!("{}/{}.cidata.iso", self.config.socket_dir, instance_id)
    }

    /// Host-side base path of the multiplexing Unix socket for the guest's
    /// vsock device. Firecracker appends `_<port>` per guest listener, so this
    /// base is what `PUT /vsock` receives and what teardown must clean up
    /// (alongside the per-port `<base>_<port>` files Firecracker creates).
    fn vsock_path(&self, instance_id: &str) -> String {
        format!("{}/{}.vsock", self.config.socket_dir, instance_id)
    }

    async fn configure_and_boot(
        &self,
        client: &FirecrackerClient,
        instance_id: &str,
        rootfs_rw: &str,
        deployment: &Deployment,
        net_alloc: Option<&InstanceNet>,
        resolved_mounts: &[ResolvedMount],
    ) -> Result<(), String> {
        // Configure the guest NIC from the kernel command line (ipconfig) rather
        // than from userspace. The kernel brings eth0 up with its address before
        // PID 1 starts, so `network-online.target` is satisfied immediately and
        // nothing in the guest has to race to assign the address. The format is
        //   ip=<client>::<gw>:<netmask>:<hostname>:<device>:off
        // (off = no autoconf). When the deployment has no network this is empty
        // and the base boot_args are used unchanged.
        let boot_args = match net_alloc {
            Some(n) => format!(
                "{} ip={}::{}:{}::eth0:off",
                self.config.boot_args,
                n.guest_ip,
                n.host_ip,
                prefix_to_netmask(n.prefix_len),
            ),
            None => self.config.boot_args.clone(),
        };

        client
            .put_boot_source(&BootSource {
                kernel_image_path: self.config.kernel_path.clone(),
                boot_args: Some(boot_args),
                initrd_path: None,
            })
            .await
            .map_err(|e| e.to_string())?;

        // rootfs is /dev/vda.
        client
            .put_drive(&Drive {
                drive_id: "rootfs".to_string(),
                path_on_host: rootfs_rw.to_string(),
                is_root_device: true,
                is_read_only: false,
            })
            .await
            .map_err(|e| e.to_string())?;

        // Volumes attach next as virtio-block devices /dev/vdb, /dev/vdc, …
        // (in declaration order, right after the root device). Each backing
        // ext4 image is built on the host; the guest mounts the device at the
        // requested destination via cloud-init.
        let guest_mounts = self
            .prepare_volume_drives(client, instance_id, deployment, resolved_mounts)
            .await?;

        // A cidata drive is attached whenever cloud-init has something to do:
        // env vars, a static network config, or volume mounts. It attaches
        // AFTER the volumes so the guest device letters for volumes are stable
        // (vdb, vdc, …); cidata takes the next free letter.
        let guest_net = net_alloc.map(|n| GuestNet {
            guest_ip: n.guest_ip.clone(),
            host_ip: n.host_ip.clone(),
            prefix_len: n.prefix_len,
            mac: n.mac.clone(),
        });
        if !deployment.environment.is_empty() || guest_net.is_some() || !guest_mounts.is_empty() {
            let socket_dir = PathBuf::from(&self.config.socket_dir);
            let iso_path = crate::hypervisor::cloud_init::build_cidata_iso(
                instance_id,
                deployment,
                &guest_mounts,
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

        // Attach a vsock device only when the deployment declares a `command`
        // health check — its sole consumer, mirroring the Cloud Hypervisor
        // runtime. The guest reaches `ring-agent` over AF_VSOCK; the host
        // reaches it through the multiplexing Unix socket at `vsock_path`.
        // Same boot-time limitation as CH: adding a `command` check to a
        // running deployment only takes effect after the next VM restart, and
        // whether one ever happens depends on the check's `on_failure`
        // (`restart` heals itself; `alert`/`stop` do not). `execute_command_probe`
        // detects a VM booted without the device and says so.
        if needs_vsock(deployment) {
            client
                .put_vsock(&Vsock {
                    vsock_id: "vsock0".to_string(),
                    guest_cid: cid_for_instance(instance_id),
                    uds_path: self.vsock_path(instance_id),
                })
                .await
                .map_err(|e| e.to_string())?;
        }

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
            .map(|info| info.pid)
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
        let console_log = self.console_log_path(instance_id);
        let _ = std::fs::remove_file(&console_log);
        // Sweep any rotated backups (`<id>.console.log.1`, `.2`, ...) the
        // rotator left behind. Stop at the first missing index.
        for idx in 1u32..=1000 {
            let backup = format!("{}.{}", console_log, idx);
            if !Path::new(&backup).exists() {
                break;
            }
            let _ = std::fs::remove_file(&backup);
        }
        // Reap ephemeral (Bind/Content) volume images. Persistent Named volumes
        // live under volumes/ and are intentionally kept.
        self.cleanup_ephemeral_volumes(instance_id);
        // Remove the vsock base socket and the per-port multiplexed socket
        // Firecracker creates (`<base>_<port>`). No-ops when the instance had
        // no vsock device.
        let vsock_base = self.vsock_path(instance_id);
        let _ = std::fs::remove_file(&vsock_base);
        let _ = std::fs::remove_file(format!("{}_{}", vsock_base, RING_AGENT_VSOCK_PORT));
        !Path::new(&socket_path).exists()
    }

    /// SIGTERM the firecracker process, then SIGKILL if it doesn't exit
    /// promptly, and wait until it's actually gone. Waiting matters for the
    /// tap: Firecracker holds the tap's backend fd while alive, so the tap
    /// can only be removed once the process has fully exited.
    async fn kill_pid(&self, pid: u32) {
        let target = Pid::from_raw(pid as i32);
        let _ = kill(target, Signal::SIGTERM);
        for i in 0..20 {
            // `kill(pid, None)` only checks existence; Err (ESRCH) means gone.
            if kill(target, None).is_err() {
                return;
            }
            if i == 5 {
                // Still alive after ~300ms — escalate.
                let _ = kill(target, Signal::SIGKILL);
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

    /// Every instance on this host, across all deployments, read from the socket
    /// directory. Unlike [`Self::scan_instances`] it does not filter by
    /// deployment: tap names are derived from a hash of the instance id, so a
    /// clash can involve two entirely unrelated deployments.
    fn all_instances(&self) -> Vec<String> {
        let entries = match std::fs::read_dir(&self.config.socket_dir) {
            Ok(e) => e,
            Err(_) => return Vec::new(),
        };
        entries
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().to_string();
                name.strip_suffix(".sock").map(|id| id.to_string())
            })
            .collect()
    }

    /// Instances of a deployment that are alive on disk (socket present) but
    /// absent from our PID map — i.e. inherited from a previous `ring-server`.
    /// These are the ones whose in-process networking state was lost on restart.
    fn orphan_instances(&self, deployment_id: &str) -> Vec<String> {
        let pids = self.pids.lock().unwrap();
        self.scan_instances(deployment_id)
            .into_iter()
            .filter(|id| !pids.contains_key(id))
            .collect()
    }

    /// Delete `tap_name` if it exists on the host but belongs to no live
    /// instance, so a fresh boot can claim the name.
    ///
    /// `TapDevice::create` refuses an existing interface — without that, a hash
    /// collision on the tap name would silently put two guests on one tap and
    /// one host IP. But `TapDevice::delete` is best-effort (it gives up on
    /// EPERM, or after exhausting its EBUSY retries), so a teardown can leave an
    /// interface behind. That leftover would then block every future boot that
    /// hashes to the same slot — trading a rare silent collision for a
    /// permanent, self-inflicted outage.
    ///
    /// Ownership is decided from the socket directory, which is Ring's inventory
    /// of what exists: a tap whose name matches no on-disk instance is
    /// unreachable garbage, safe to remove. A tap that *does* match a live
    /// instance is left alone, and `create` will then fail with a clear message
    /// — the genuine collision case, where refusing is the right answer.
    fn reclaim_stale_tap(&self, tap_name: &str, booting_instance_id: &str) {
        if !TapDevice::exists(tap_name) {
            return;
        }

        // Every instance Ring currently knows about, across all deployments.
        // The booting instance's own socket already exists at this point, and it
        // hashes to this very tap name — counting it would make the interface
        // look claimed and defeat the reclaim.
        let claimed = self
            .all_instances()
            .into_iter()
            .filter(|id| id != booting_instance_id)
            .any(|id| InstanceNet::for_instance(&id).tap_name == tap_name);
        if claimed {
            return;
        }

        // The socket inventory can lie: a live VM whose socket was unlinked
        // would look unowned, and deleting its interface would cut the network
        // of a running guest. Confirm against /proc, which is the actual source
        // of truth for "is a VM still using this tap", before removing anything.
        if live_vm_uses_tap(tap_name, booting_instance_id) {
            warn!(
                "Firecracker: tap '{}' has no instance socket but a live VM still uses it — \
                 leaving it alone",
                tap_name
            );
            return;
        }

        warn!(
            "Firecracker: reclaiming stale tap '{}' (no live instance owns it)",
            tap_name
        );
        TapDevice::adopt(tap_name).delete();

        // `delete` is best-effort: it gives up on EPERM (no CAP_NET_ADMIN) and
        // after exhausting its EBUSY retries. If the interface is still there,
        // `TapDevice::create` will refuse the boot — say why now, since its
        // message alone would read as a hash collision when it is really a
        // permissions or teardown problem. The remedy it prints
        // (`ip link delete <name>`) applies either way.
        if TapDevice::exists(tap_name) {
            warn!(
                "Firecracker: could not remove stale tap '{}' — the boot will fail. \
                 Check that ring-server holds CAP_NET_ADMIN, then delete it manually",
                tap_name
            );
        }
    }

    /// Re-adopt the host networking of instances inherited from a previous
    /// `ring-server` (socket on disk, but no PID in our map). The VM and its
    /// persistent tap survive a restart, but the socat forwarders — children of
    /// the old process — died with it, so a deployment with `ports` loses its
    /// host port-forwarding. Re-derive the network from the instance id (the IP
    /// and tap name are pure functions of it), re-adopt the tap, re-spawn one
    /// socat per published port, and re-register the PID so the instance is
    /// fully owned again. Nothing is persisted: every input is either
    /// deterministic or already in the deployment. No-op for portless
    /// deployments (no network to lose) and for instances we already track.
    async fn readopt_networking(&self, deployment: &Deployment) {
        if deployment.ports.is_empty() {
            return;
        }
        for instance_id in self.orphan_instances(&deployment.id) {
            // Skip if we already re-adopted its forwarders on an earlier tick.
            if self
                .port_forwarders
                .lock()
                .unwrap()
                .contains_key(&instance_id)
            {
                continue;
            }
            let net = InstanceNet::for_instance(&instance_id);
            // The tap is persistent, so it's still on the host — adopt by name
            // and bring it back under our ownership. ensure_outbound_nat is
            // idempotent and global, so re-running it after a restart is a no-op.
            let tap = TapDevice::adopt(&net.tap_name);
            crate::hypervisor::host_nat::ensure_outbound_nat();

            let mut forwarders = Vec::with_capacity(deployment.ports.len());
            let mut ok = true;
            for p in &deployment.ports {
                // A forwarder orphaned by an unclean exit of the previous
                // ring-server is reparented to init and still holds the port;
                // kill it first so the re-spawn below doesn't hit "address
                // already in use". No-op when the old server SIGKILLed it or
                // exited cleanly (Drop killed it).
                port_forwarder::kill_orphan_forwarder(
                    p.host_ip.as_deref(),
                    p.published,
                    p.protocol,
                );
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
                        warn!(
                            "Firecracker: failed to re-adopt port {} for {} after restart: {}",
                            p.published, instance_id, e
                        );
                        ok = false;
                        break;
                    }
                }
            }
            if !ok {
                // Drop partial forwarders; leave the instance for the next tick
                // to retry rather than tearing down a live VM over a bind race.
                continue;
            }

            self.taps.lock().unwrap().insert(instance_id.clone(), tap);
            if !forwarders.is_empty() {
                self.port_forwarders
                    .lock()
                    .unwrap()
                    .insert(instance_id.clone(), forwarders);
            }
            if let Some(pid) = find_pid_by_socket(&self.socket_path(&instance_id)) {
                let (_, mem_mib) = parse_resources(deployment);
                let memory_limit_bytes = (mem_mib as u64).saturating_mul(1024 * 1024);
                self.pids.lock().unwrap().insert(
                    instance_id.clone(),
                    InstanceProcessInfo {
                        pid,
                        memory_limit_bytes,
                    },
                );
            }
            info!(
                "Firecracker: re-adopted networking for {} after restart",
                instance_id
            );
        }
    }

    async fn handle_worker_deployment(
        &self,
        mut deployment: Deployment,
        resolved_mounts: &[ResolvedMount],
    ) -> Deployment {
        // Before scaling, re-adopt any instance inherited from a previous
        // ring-server so its network is restored without recreating the VM.
        self.readopt_networking(&deployment).await;

        let current = self.scan_instances(&deployment.id);
        let desired = deployment.replicas as usize;

        if current.len() < desired {
            for _ in current.len()..desired {
                match self.start_vm(&deployment, resolved_mounts).await {
                    Ok(_) => {}
                    Err(e) => {
                        error!("Firecracker: failed to start instance: {}", e);
                        // This path used to set the status without ever touching
                        // `restart_count`, so the scheduler — which keeps polling
                        // `create_container_error` — retried the boot forever.
                        let terminal = classify_vm_start_error(&e).0.is_some();
                        apply_vm_start_failure(
                            &mut deployment,
                            &e,
                            "firecracker",
                            DeploymentStatus::CrashLoopBackOff,
                        );
                        // A transient failure that still has budget left keeps
                        // reporting a create error, so the retry stays visible
                        // instead of silently sitting in Creating. A terminal
                        // verdict already owns the status and must not be
                        // overwritten here.
                        if !terminal && deployment.restart_count < MAX_RESTART_COUNT {
                            deployment.status = DeploymentStatus::CreateContainerError;
                        }
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

        // Promote to Running only after confirming at least one instance is
        // genuinely alive (API socket on disk AND a live firecracker process).
        // `start_vm` returning Ok means the boot was *accepted*, not that the
        // guest stayed up — a VM that crashes right after boot leaves a stale
        // socket, which `scan_instances` alone would mistake for a healthy
        // instance and wrongly report Running. Gating on `instance_alive` closes
        // that window; if nothing is alive we leave the prior status (typically
        // Creating) so the deployment reconciles again next tick instead of
        // flapping through a false Running.
        let any_alive = deployment
            .instances
            .iter()
            .any(|id| self.instance_alive(id));
        if let Some(status) = liveness_confirmed_status(&deployment.status, any_alive) {
            deployment.status = status;
        }
        deployment
    }

    /// Whether an instance is genuinely alive: its API socket is on disk AND a
    /// `firecracker` process is still bound to it. Firecracker removes the
    /// socket on a clean exit (guest poweroff), but a crash can leave a stale
    /// socket behind — so socket presence alone is not "running". Used by the
    /// job path to tell "still running" from "guest powered off / crashed".
    fn instance_alive(&self, instance_id: &str) -> bool {
        let socket = self.socket_path(instance_id);
        Path::new(&socket).exists() && find_pid_by_socket(&socket).is_some()
    }

    /// Run a `kind: job` deployment: boot a single VM and mark it `Completed`
    /// once the guest powers off. Firecracker exposes no VM-state API like
    /// Cloud Hypervisor's `info()`, but the guest powering off makes the
    /// `firecracker` process exit, which is an equally clean signal — a job
    /// that was `Running` and now has no live process has completed.
    ///
    /// `replicas` is ignored (a job is one VM). Because the guest's main-process
    /// exit code isn't surfaced, any clean shutdown is treated as success —
    /// same convention (“Approach A”) as the Cloud Hypervisor runtime.
    async fn handle_job_deployment(
        &self,
        mut deployment: Deployment,
        resolved_mounts: &[ResolvedMount],
    ) -> Deployment {
        // Terminal states are sticky: never reboot a finished job.
        if matches!(
            deployment.status,
            DeploymentStatus::Completed
                | DeploymentStatus::Failed
                | DeploymentStatus::CrashLoopBackOff
        ) {
            return deployment;
        }

        // Re-adopt across a restart so an inherited job VM isn't double-booted.
        self.readopt_networking(&deployment).await;

        let instance = self.scan_instances(&deployment.id).into_iter().next();

        if let Some(instance_id) = instance {
            if self.instance_alive(&instance_id) {
                // Still running.
                deployment.instances = vec![instance_id];
                if matches!(
                    deployment.status,
                    DeploymentStatus::Creating | DeploymentStatus::Pending
                ) {
                    deployment.status = DeploymentStatus::Running;
                }
            } else {
                // Socket on disk but no live process: the guest powered off (or
                // the VM crashed). Either way the job is done — finalize and
                // sweep the leftover socket/rootfs/console artifacts.
                info!(
                    "Firecracker job VM {} has no live process, finalizing as Completed",
                    instance_id
                );
                self.stop_vm(&instance_id).await;
                deployment.instances.clear();
                deployment.status = DeploymentStatus::Completed;
                deployment.emit_event(
                    "info",
                    format!("Job VM {} completed", instance_id),
                    "firecracker",
                    Some("job_completed"),
                );
            }
        } else if deployment.status == DeploymentStatus::Running {
            // Was Running, now nothing on disk: firecracker exited cleanly on
            // guest poweroff and took its socket with it. A clean exit is
            // success.
            info!(
                "Firecracker job deployment {} has no live VM after Running; finalizing as Completed",
                deployment.id
            );
            deployment.instances.clear();
            deployment.status = DeploymentStatus::Completed;
            deployment.emit_event(
                "info",
                "Job VM exited and firecracker terminated; finalized as completed".to_string(),
                "firecracker",
                Some("job_completed"),
            );
        } else if matches!(
            deployment.status,
            DeploymentStatus::Creating | DeploymentStatus::Pending
        ) {
            // No VM yet: boot exactly one.
            match self.start_vm(&deployment, resolved_mounts).await {
                Ok(instance_id) => {
                    deployment.instances = vec![instance_id];
                    deployment.status = DeploymentStatus::Running;
                }
                Err(e) => {
                    error!(
                        "Firecracker: failed to start job VM for deployment {}: {}",
                        deployment.id, e
                    );
                    apply_vm_start_failure(
                        &mut deployment,
                        &e,
                        "firecracker",
                        DeploymentStatus::Failed,
                    );
                }
            }
        }

        deployment
    }
}

/// Decide the post-scale worker status from the current status and whether any
/// instance is genuinely alive. Returns `Some(Running)` only when liveness is
/// confirmed; `None` leaves the status untouched (so a VM that booted then died
/// is not falsely reported Running, and an existing status — including a prior
/// `CreateContainerError` from a failed `start_vm` — is preserved).
///
/// `start_vm` returning Ok means the boot was *accepted*, not that the guest
/// stayed up; this gate is what makes "Running" mean "actually running".
fn liveness_confirmed_status(
    current: &DeploymentStatus,
    any_alive: bool,
) -> Option<DeploymentStatus> {
    // A status the scale loop just set from a failed boot must survive this
    // gate. With replicas > 1 one instance can be alive while another failed to
    // start, and reporting Running would erase the failure the operator needs to
    // see — including the terminal verdicts the classifier now produces.
    if matches!(
        current,
        DeploymentStatus::CreateContainerError
            | DeploymentStatus::CrashLoopBackOff
            | DeploymentStatus::ImagePullBackOff
            | DeploymentStatus::ConfigError
            | DeploymentStatus::InsufficientResources
            | DeploymentStatus::Failed
    ) {
        return None;
    }
    any_alive.then_some(DeploymentStatus::Running)
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

/// Every API socket path currently served by a live `firecracker` process, read
/// in one pass over `/proc`.
///
/// Filtering a listing by liveness would otherwise call `instance_alive` per
/// instance, and each of those walks all of `/proc` — N full scans for N
/// replicas. One scan answers the question for every instance at once.
///
/// Keyed on the full socket path, not the instance id: two `ring-server`
/// instances with different `socket_dir`s can mint the same instance id, and
/// matching on the basename alone would let one of them mark the other's stale
/// socket as live. This mirrors the exact-path comparison `instance_alive` does.
fn live_socket_paths() -> std::collections::HashSet<String> {
    let mut live = std::collections::HashSet::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return live;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        let Ok(cmdline) = std::fs::read(format!("/proc/{}/cmdline", pid)) else {
            continue;
        };
        if let Some(path) = socket_path_from_cmdline(&cmdline) {
            live.insert(path);
        }
    }
    live
}

/// Translate the runtime-agnostic `status` filter of
/// [`RuntimeLifecycle::list_instances`] into the liveness an instance must have
/// to satisfy it: `Some(true)` for alive, `Some(false)` for dead, `None` for no
/// filter at all.
///
/// The filter vocabulary is Docker's, since that is what the trait's callers
/// speak. Firecracker exposes no VM-state API, so the only observable
/// distinction is whether a live process still backs the instance — `active` and
/// `running` collapse onto the same thing here, and `exited` is its negation.
///
/// The argument used to be ignored entirely, so every caller got every instance
/// with a socket on disk, including the stale sockets crashed VMs leave behind.
fn fc_liveness_for_status(status: &str) -> FcFilter {
    match status {
        "all" => FcFilter::Any,
        "active" | "running" => FcFilter::Alive(true),
        "exited" | "stopped" => FcFilter::Alive(false),
        // Unknown filter: match nothing. Falling back to "everything" would
        // misreport a deployment as fully up, and falling back to "dead" would
        // be just as wrong — neither is what the caller asked for.
        _ => FcFilter::None,
    }
}

/// What [`fc_liveness_for_status`] resolved a status filter to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FcFilter {
    /// No filtering — every instance with a socket on disk.
    Any,
    /// Keep instances whose liveness matches the flag.
    Alive(bool),
    /// Unrecognised filter: keep nothing.
    None,
}

/// Is a live `firecracker` process currently serving the instance that owns
/// `tap_name`? Scans `/proc` for firecracker processes and re-derives each
/// one's tap name from the instance id embedded in its `--api-sock` path.
///
/// This is the safety net over the socket inventory used by
/// [`FirecrackerLifecycle::reclaim_stale_tap`]: a running VM whose socket file
/// was removed would vanish from that inventory, and reclaiming its tap would
/// take the network away from a live guest. `/proc` still shows the process.
///
/// `booting_instance_id` is excluded — the process being booted right now owns
/// the very socket whose tap we are about to claim.
///
/// Errs on the side of "in use" whenever `/proc` cannot be read, so on a host
/// with `hidepid` the reclaim simply does not happen and the operator gets the
/// manual remedy from `TapDevice::create`. Failing to reclaim costs one boot;
/// deleting a live VM's interface breaks a running workload.
fn live_vm_uses_tap(tap_name: &str, booting_instance_id: &str) -> bool {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        // Can't read /proc: assume the tap is in use rather than risk deleting
        // a live VM's interface.
        return true;
    };

    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        let cmdline = match std::fs::read(format!("/proc/{}/cmdline", pid)) {
            Ok(c) => c,
            // The process exited between readdir and here — genuinely gone, so
            // it owns nothing.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            // Anything else (EACCES under `hidepid`, EPERM, I/O error) means a
            // process we cannot inspect, not a process that isn't there. Treat
            // the tap as in use rather than risk cutting a live guest's network.
            Err(_) => {
                warn!(
                    "Firecracker: cannot inspect /proc/{}/cmdline; assuming tap '{}' is in use",
                    pid, tap_name
                );
                return true;
            }
        };
        let Some(instance_id) = instance_id_from_cmdline(&cmdline) else {
            continue;
        };
        if instance_id == booting_instance_id {
            continue;
        }
        if InstanceNet::for_instance(&instance_id).tap_name == tap_name {
            return true;
        }
    }
    false
}

/// The API socket path of the firecracker process described by `cmdline`.
/// `None` when this is not a firecracker process or carries no socket argument.
fn socket_path_from_cmdline(cmdline: &[u8]) -> Option<String> {
    let mut args = cmdline.split(|&b| b == 0);
    if !args.next()?.ends_with(b"firecracker") {
        return None;
    }
    args.filter_map(|arg| std::str::from_utf8(arg).ok())
        .find(|s| s.ends_with(".sock"))
        .map(|s| s.to_string())
}

/// The instance id of the firecracker process described by `cmdline`, taken from
/// the file stem of its `--api-sock` argument (`/run/fc/<instance-id>.sock`).
/// `None` when this is not a firecracker process or carries no socket argument.
fn instance_id_from_cmdline(cmdline: &[u8]) -> Option<String> {
    let path = socket_path_from_cmdline(cmdline)?;
    let stem = path.strip_suffix(".sock")?;
    stem.rsplit('/').next().map(|id| id.to_string())
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
/// AF_VSOCK port `ring-agent` listens on inside the guest. Must match
/// `crates/ring-agent` and `hypervisor::vsock_client::VSOCK_PORT`. Used here
/// only to clean up the per-port multiplexing socket Firecracker creates.
const RING_AGENT_VSOCK_PORT: u32 = 2375;

/// A deployment needs a vsock device iff it declares a `command` health check
/// — the only consumer of the in-guest agent today. Mirrors the gate in the
/// Cloud Hypervisor runtime so the two behave identically.
fn needs_vsock(deployment: &Deployment) -> bool {
    deployment
        .health_checks
        .iter()
        .any(|hc| matches!(hc, HealthCheck::Command { .. }))
}

/// Render a CIDR prefix length as a dotted-quad netmask (e.g. 30 -> 255.255.255.252)
/// for the kernel ipconfig `ip=` parameter, which wants the mask in that form.
fn prefix_to_netmask(prefix_len: u8) -> String {
    let bits: u32 = if prefix_len >= 32 {
        u32::MAX
    } else {
        u32::MAX.checked_shl(32 - prefix_len as u32).unwrap_or(0)
    };
    format!(
        "{}.{}.{}.{}",
        (bits >> 24) & 0xff,
        (bits >> 16) & 0xff,
        (bits >> 8) & 0xff,
        bits & 0xff
    )
}

#[cfg(test)]
mod netmask_tests {
    use super::prefix_to_netmask;

    #[test]
    fn prefix_30_is_a_four_host_mask() {
        assert_eq!(prefix_to_netmask(30), "255.255.255.252");
    }

    #[test]
    fn prefix_24_and_32() {
        assert_eq!(prefix_to_netmask(24), "255.255.255.0");
        assert_eq!(prefix_to_netmask(32), "255.255.255.255");
    }
}

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
        resolved_mounts: Vec<ResolvedMount>,
    ) -> Deployment {
        if deployment.status == DeploymentStatus::Deleted {
            // Re-adopt first so an instance inherited from a previous
            // ring-server (its forwarders orphaned, not in our maps) is brought
            // back under ownership — then stop_vm's Drop kills its socat. Without
            // this, deleting right after a restart could leak an orphan forwarder
            // still holding the port.
            self.readopt_networking(&deployment).await;
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
            self.handle_job_deployment(deployment, &resolved_mounts)
                .await
        } else {
            self.handle_worker_deployment(deployment, &resolved_mounts)
                .await
        }
    }

    async fn list_instances(&self, deployment_id: String, status: &str) -> Vec<String> {
        let ids = self.scan_instances(&deployment_id);
        match fc_liveness_for_status(status) {
            // "all" — every instance with a socket on disk, alive or not.
            FcFilter::Any => ids,
            // Firecracker has no VM-state API: an instance is either backed by a
            // live process or it is not. `instance_alive` is that distinction,
            // and it also filters out the stale sockets a crashed VM leaves
            // behind — which a bare socket scan would report as running.
            FcFilter::Alive(want) => {
                // Index /proc once for the whole listing. Calling
                // `instance_alive` per id would rescan every process for each
                // instance — N full /proc walks for N replicas.
                let live = live_socket_paths();
                ids.into_iter()
                    .filter(|id| {
                        // Same test as `instance_alive`: the socket is on disk
                        // AND a live process is bound to that exact path.
                        let socket = self.socket_path(id);
                        let alive = Path::new(&socket).exists() && live.contains(&socket);
                        alive == want
                    })
                    .collect()
            }
            FcFilter::None => Vec::new(),
        }
    }

    async fn remove_instance(&self, instance_id: String) -> bool {
        self.stop_vm(&instance_id).await
    }

    /// Read the persisted serial console for every instance of the deployment.
    /// Scans disk (not just our PID map) so a crashed or restart-inherited VM's
    /// console is still readable. Reads through rotated backups via the shared
    /// `console_logs` helper.
    async fn get_logs(
        &self,
        deployment_id: &str,
        tail: Option<&str>,
        since: Option<i32>,
        instance_filter: Option<&str>,
    ) -> Vec<Log> {
        let mut logs = Vec::new();
        for instance_id in self.scan_instances(deployment_id) {
            if let Some(want) = instance_filter
                && instance_id != want
            {
                continue;
            }
            let path = PathBuf::from(self.console_log_path(&instance_id));
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

    /// Follow the serial console of every matching instance, equivalent to
    /// `tail -f`, emitting each new line as an SSE event. Mirrors
    /// `get_logs` instance selection (disk scan + optional filter).
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

        let filtered: Vec<String> = self
            .scan_instances(deployment_id)
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
            let path = PathBuf::from(self.console_log_path(&instance_id));
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

    /// Run a `command` health check inside the guest via `ring-agent` over the
    /// Firecracker vsock-over-Unix-socket transport. Mirrors the Cloud
    /// Hypervisor implementation; only the transport differs (`exec_uds` does
    /// Firecracker's `CONNECT` handshake before speaking the agent protocol).
    async fn execute_command_probe(
        &self,
        instance_id: &str,
        command: &str,
    ) -> (HealthCheckStatus, Option<String>) {
        let cid = cid_for_instance(instance_id);
        let uds_path = self.vsock_path(instance_id);
        let argv = vec!["/bin/sh".to_string(), "-c".to_string(), command.to_string()];
        let timeout = std::time::Duration::from_secs(30);

        match vsock_client::exec_uds(cid, &uds_path, &argv, &[], timeout).await {
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
            Err(VsockError::Connect { cid, source }) => {
                let host_socket_present = Path::new(&self.vsock_path(instance_id)).exists();
                (
                    HealthCheckStatus::Failed,
                    Some(vsock_client::connect_failure_message(
                        "firecracker",
                        cid,
                        &source.to_string(),
                        host_socket_present,
                    )),
                )
            }
            Err(e) => (
                HealthCheckStatus::Failed,
                Some(format!("vsock probe failed: {}", e)),
            ),
        }
    }

    /// Per-instance CPU / memory / network / disk / pid stats for every running
    /// instance of the deployment. Reads host-side `/proc/<pid>/*` and the
    /// per-VM tap counters via the shared `stats` helpers — same source and
    /// semantics as Cloud Hypervisor. Instances we don't have a PID for (e.g.
    /// inherited across a restart before `readopt_networking` ran) are skipped.
    async fn get_instance_stats(
        &self,
        deployment_id: &str,
    ) -> Vec<crate::api::dto::stats::InstanceStatsOutput> {
        let instances = self.scan_instances(deployment_id);
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
/// VM, short enough that an HTTP `metrics` call doesn't feel laggy. Matches
/// the Cloud Hypervisor runtime and Docker's ~1s frame cadence.
const CPU_SAMPLE_INTERVAL_MS: u64 = 500;

impl FirecrackerLifecycle {
    /// The tracked process info for an instance, or `None` if this server
    /// didn't boot it (no PID — typical after a ring-server restart until
    /// networking is re-adopted).
    fn process_info(&self, instance_id: &str) -> Option<InstanceProcessInfo> {
        self.pids.lock().ok()?.get(instance_id).copied()
    }

    /// Sample CPU twice with a short delay, read RSS once, and assemble the
    /// `InstanceStatsOutput`. Returns `None` if the process tracking entry is
    /// gone (VM stopped, or this server didn't spawn the VM).
    async fn read_instance_stats(
        &self,
        instance_id: &str,
    ) -> Option<crate::api::dto::stats::InstanceStatsOutput> {
        let info = self.process_info(instance_id)?;

        let prev = crate::hypervisor::stats::read_cpu_sample(info.pid).await?;
        tokio::time::sleep(tokio::time::Duration::from_millis(CPU_SAMPLE_INTERVAL_MS)).await;
        let curr = crate::hypervisor::stats::read_cpu_sample(info.pid).await?;

        let interval_secs = CPU_SAMPLE_INTERVAL_MS as f64 / 1000.0;
        // SC_CLK_TCK is fixed at compile time on Linux (typically 100).
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

    /// A confirmed-alive instance promotes a fresh deployment to Running.
    #[test]
    fn liveness_confirmed_promotes_to_running() {
        assert_eq!(
            liveness_confirmed_status(&DeploymentStatus::Creating, true),
            Some(DeploymentStatus::Running)
        );
    }

    /// A VM that booted then died (nothing alive) must NOT be reported Running:
    /// the status is left untouched so the deployment reconciles again.
    #[test]
    fn no_live_instance_does_not_promote() {
        assert_eq!(
            liveness_confirmed_status(&DeploymentStatus::Creating, false),
            None
        );
    }

    /// A failed `start_vm` (CreateContainerError) is never overwritten by the
    /// liveness gate, even if a stale socket made `any_alive` look true.
    #[test]
    fn create_error_is_preserved() {
        assert_eq!(
            liveness_confirmed_status(&DeploymentStatus::CreateContainerError, true),
            None
        );
    }

    /// Same reasoning for every terminal verdict the classifier can produce on a
    /// failed boot: with replicas > 1 a sibling instance can be alive, and
    /// promoting to Running there would erase the failure entirely.
    #[test]
    fn terminal_boot_failures_are_preserved() {
        for status in [
            DeploymentStatus::CrashLoopBackOff,
            DeploymentStatus::ImagePullBackOff,
            DeploymentStatus::ConfigError,
            DeploymentStatus::InsufficientResources,
            DeploymentStatus::Failed,
        ] {
            assert_eq!(
                liveness_confirmed_status(&status, true),
                None,
                "{:?} must survive the liveness gate",
                status
            );
        }
    }

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

    /// `all_instances` must span every deployment, unlike `scan_instances`:
    /// tap names come from a hash of the instance id, so a clash can involve two
    /// unrelated deployments. Missing one would make a live tap look orphaned
    /// and get it deleted out from under a running VM.
    #[test]
    fn all_instances_spans_every_deployment() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("ring-fc-all-{}-{}", std::process::id(), nanos));
        std::fs::create_dir_all(&dir).unwrap();

        let cfg = FirecrackerRuntimeConfig {
            socket_dir: dir.to_string_lossy().to_string(),
            ..FirecrackerRuntimeConfig::default()
        };
        let lc = FirecrackerLifecycle::new(cfg);

        for f in [
            "dep-1-aaa.sock",
            "dep-2-bbb.sock",
            "dep-1-aaa.ext4", // not a socket
        ] {
            std::fs::write(dir.join(f), b"").unwrap();
        }

        let mut found = lc.all_instances();
        found.sort();
        assert_eq!(
            found,
            vec!["dep-1-aaa".to_string(), "dep-2-bbb".to_string()],
            "must cross deployment boundaries and ignore non-sockets"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A tap owned by a live instance must never be reclaimed. The reclaim is
    /// keyed on the socket inventory, so this asserts the ownership lookup that
    /// decides it — deleting a live VM's interface would be far worse than the
    /// collision the guard exists to catch.
    #[test]
    fn a_live_instance_claims_its_tap_name() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("ring-fc-claim-{}-{}", std::process::id(), nanos));
        std::fs::create_dir_all(&dir).unwrap();

        let cfg = FirecrackerRuntimeConfig {
            socket_dir: dir.to_string_lossy().to_string(),
            ..FirecrackerRuntimeConfig::default()
        };
        let lc = FirecrackerLifecycle::new(cfg);

        let live = "dep-1-live";
        std::fs::write(dir.join(format!("{}.sock", live)), b"").unwrap();
        let live_tap = InstanceNet::for_instance(live).tap_name;

        // The live instance claims its own tap name...
        assert!(
            lc.all_instances()
                .iter()
                .any(|id| InstanceNet::for_instance(id).tap_name == live_tap)
        );

        // ...but excluding it (as the booting instance is) leaves the name
        // unclaimed, which is exactly what makes a leftover reclaimable.
        assert!(
            !lc.all_instances()
                .iter()
                .filter(|id| *id != live)
                .any(|id| InstanceNet::for_instance(id).tap_name == live_tap)
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn orphan_instances_are_on_disk_but_not_in_pid_map() {
        // After a restart the pid map is empty, so every on-disk instance is an
        // orphan whose networking must be re-adopted. An instance we still track
        // in `pids` (booted by this process) is not an orphan.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("ring-fc-orphan-{}-{}", std::process::id(), nanos));
        std::fs::create_dir_all(&dir).unwrap();

        let cfg = FirecrackerRuntimeConfig {
            socket_dir: dir.to_string_lossy().to_string(),
            ..FirecrackerRuntimeConfig::default()
        };
        let lc = FirecrackerLifecycle::new(cfg);

        for f in ["dep-1-aaa.sock", "dep-1-bbb.sock"] {
            std::fs::write(dir.join(f), b"").unwrap();
        }

        // Empty pid map (post-restart): both instances are orphans.
        let mut orphans = lc.orphan_instances("dep-1");
        orphans.sort();
        assert_eq!(
            orphans,
            vec!["dep-1-aaa".to_string(), "dep-1-bbb".to_string()]
        );

        // One instance is tracked again (re-adopted or freshly booted): no
        // longer an orphan.
        lc.pids.lock().unwrap().insert(
            "dep-1-aaa".to_string(),
            InstanceProcessInfo {
                pid: 4242,
                memory_limit_bytes: 0,
            },
        );
        assert_eq!(lc.orphan_instances("dep-1"), vec!["dep-1-bbb".to_string()]);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn cmdline_matches_socket_exact_arg() {
        let sock = "/run/fc/dep-1-aaa.sock";
        // argv[0]=firecracker, then --api-sock <sock>
        let cmd = b"/usr/bin/firecracker\0--api-sock\0/run/fc/dep-1-aaa.sock\0";
        assert!(cmdline_matches_socket(cmd, sock));
    }

    /// The status argument used to be ignored, so every caller got every
    /// instance with a socket on disk — including the stale sockets crashed VMs
    /// leave behind, which then looked like running instances.
    #[test]
    fn status_filter_maps_to_liveness() {
        assert_eq!(fc_liveness_for_status("all"), FcFilter::Any);
        // Firecracker has no VM-state API, so active and running are the same
        // observable thing: a live process backs the instance.
        assert_eq!(fc_liveness_for_status("active"), FcFilter::Alive(true));
        assert_eq!(fc_liveness_for_status("running"), FcFilter::Alive(true));
        assert_eq!(fc_liveness_for_status("exited"), FcFilter::Alive(false));
        assert_eq!(fc_liveness_for_status("stopped"), FcFilter::Alive(false));
    }

    /// Liveness is keyed on the full socket path, not the instance id: two
    /// `ring-server` instances with different `socket_dir`s can mint the same
    /// id, and matching on the basename alone would let one mark the other's
    /// stale socket as live.
    #[test]
    fn socket_path_is_captured_whole() {
        let cmd = b"/usr/bin/firecracker\0--api-sock\0/run/fc/dep-1-aaa.sock\0";
        assert_eq!(
            socket_path_from_cmdline(cmd).as_deref(),
            Some("/run/fc/dep-1-aaa.sock"),
            "the directory must be kept, not just the file name"
        );
        // Same instance id under a different socket_dir is a different socket.
        let other = b"/usr/bin/firecracker\0--api-sock\0/var/run/other/dep-1-aaa.sock\0";
        assert_ne!(
            socket_path_from_cmdline(cmd),
            socket_path_from_cmdline(other)
        );
    }

    #[test]
    fn socket_path_ignores_non_firecracker_processes() {
        let cmd = b"/usr/bin/socat\0--api-sock\0/run/fc/dep-1-aaa.sock\0";
        assert_eq!(socket_path_from_cmdline(cmd), None);
    }

    /// An unrecognised filter must match nothing. Returning everything would
    /// report a deployment as fully up; returning the dead ones would be just as
    /// wrong. Neither is what the caller asked for.
    #[test]
    fn unknown_status_filter_matches_nothing() {
        assert_eq!(fc_liveness_for_status("paused"), FcFilter::None);
        assert_eq!(fc_liveness_for_status(""), FcFilter::None);
    }

    /// The instance id is what lets a live VM be matched to the tap it owns, so
    /// a running guest's interface is never reclaimed. It comes from the socket
    /// argument's file stem.
    #[test]
    fn instance_id_is_read_from_the_socket_argument() {
        let cmd = b"/usr/bin/firecracker\0--api-sock\0/run/fc/dep-1-aaa.sock\0";
        assert_eq!(
            instance_id_from_cmdline(cmd).as_deref(),
            Some("dep-1-aaa"),
            "must strip both the directory and the .sock suffix"
        );
    }

    /// A non-firecracker process must never be mistaken for a VM — otherwise an
    /// unrelated process could make a genuinely stale tap look alive and block
    /// the reclaim forever.
    #[test]
    fn instance_id_ignores_non_firecracker_processes() {
        let cmd = b"/usr/bin/socat\0--api-sock\0/run/fc/dep-1-aaa.sock\0";
        assert_eq!(instance_id_from_cmdline(cmd), None);
    }

    #[test]
    fn instance_id_is_none_without_a_socket_argument() {
        let cmd = b"/usr/bin/firecracker\0--version\0";
        assert_eq!(instance_id_from_cmdline(cmd), None);
    }

    /// The id feeds straight back into the tap-name derivation, so this closes
    /// the loop the reclaim actually relies on.
    #[test]
    fn instance_id_round_trips_to_its_tap_name() {
        let id = "dep-1-aaa";
        let cmd = b"/usr/bin/firecracker\0--api-sock\0/run/fc/dep-1-aaa.sock\0";
        let parsed = instance_id_from_cmdline(cmd).expect("id parses");
        assert_eq!(
            InstanceNet::for_instance(&parsed).tap_name,
            InstanceNet::for_instance(id).tap_name
        );
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
            max_console_log_bytes: None,
            max_console_log_backups: None,
        };
        let cfg = FirecrackerRuntimeConfig::from_user_config(&user);
        assert_eq!(cfg.binary_path, "/opt/fc/firecracker");
        assert_eq!(cfg.socket_dir, "/var/run/fc");
        // Unset fields fall back to defaults.
        let defaults = FirecrackerRuntimeConfig::default();
        assert_eq!(cfg.kernel_path, defaults.kernel_path);
        assert_eq!(cfg.boot_args, defaults.boot_args);
        assert_eq!(cfg.max_console_log_bytes, defaults.max_console_log_bytes);
        assert_eq!(
            cfg.max_console_log_backups,
            defaults.max_console_log_backups
        );
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

    fn lifecycle_with_scratch_dir(label: &str) -> (FirecrackerLifecycle, std::path::PathBuf) {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "ring-fc-{}-{}-{}",
            label,
            std::process::id(),
            nanos
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = FirecrackerRuntimeConfig {
            socket_dir: dir.to_string_lossy().to_string(),
            ..FirecrackerRuntimeConfig::default()
        };
        (FirecrackerLifecycle::new(cfg), dir)
    }

    #[test]
    fn cleanup_reaps_sparse_ephemeral_images_and_stage_dirs() {
        let (lc, dir) = lifecycle_with_scratch_dir("vol-cleanup");
        let id = "inst-abcd";

        // Named-then-Bind ordering: vol0 (named) has NO ephemeral file, vol1
        // (bind) does. The old index-walk broke at vol0 and leaked vol1.
        let vol1 = dir.join(format!("{}.vol1.ext4", id));
        std::fs::write(&vol1, b"ext4").unwrap();
        // An orphaned staging dir from a crash mid-build.
        let stage = dir.join(format!("{}.vol2.stage", id));
        std::fs::create_dir_all(&stage).unwrap();
        // A persistent named image and another instance's file must survive.
        std::fs::create_dir_all(dir.join("volumes/ns")).unwrap();
        let named = dir.join("volumes/ns/data.ext4");
        std::fs::write(&named, b"keep").unwrap();
        let other = dir.join("inst-zzzz.vol0.ext4");
        std::fs::write(&other, b"keep").unwrap();

        lc.cleanup_ephemeral_volumes(id);

        assert!(!vol1.exists(), "sparse ephemeral image must be reaped");
        assert!(!stage.exists(), "orphaned staging dir must be reaped");
        assert!(named.exists(), "named volume must persist");
        assert!(other.exists(), "another instance's image must be untouched");

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
            image: "/tmp/does-not-exist.ext4".to_string(),
            config: None,
            runtime: "firecracker".to_string(),
            kind: "job".to_string(),
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
            network: None,
        }
    }

    #[tokio::test]
    async fn job_terminal_status_is_sticky_completed() {
        let (lc, dir) = lifecycle_with_scratch_dir("job-completed");
        let dep = job_deployment(DeploymentStatus::Completed);
        let out = lc.handle_job_deployment(dep, &[]).await;
        assert_eq!(out.status, DeploymentStatus::Completed);
        assert!(out.instances.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn job_terminal_status_is_sticky_failed() {
        let (lc, dir) = lifecycle_with_scratch_dir("job-failed");
        let dep = job_deployment(DeploymentStatus::Failed);
        let out = lc.handle_job_deployment(dep, &[]).await;
        assert_eq!(out.status, DeploymentStatus::Failed);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn job_running_with_no_live_vm_completes() {
        // A job that was Running but has nothing on disk: the guest powered off
        // and firecracker exited, taking its socket with it → Completed.
        let (lc, dir) = lifecycle_with_scratch_dir("job-running-gone");
        let dep = job_deployment(DeploymentStatus::Running);
        let out = lc.handle_job_deployment(dep, &[]).await;
        assert_eq!(out.status, DeploymentStatus::Completed);
        assert!(out.instances.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn job_missing_kernel_does_not_complete_silently() {
        // Creating with no kernel/rootfs on disk: start_vm fails. The default
        // kernel path doesn't exist in the scratch dir, so this is a transient
        // VmStartFailed → bumps restart_count, never silently Completed.
        let (lc, dir) = lifecycle_with_scratch_dir("job-no-kernel");
        let dep = job_deployment(DeploymentStatus::Creating);
        let out = lc.handle_job_deployment(dep, &[]).await;
        assert_ne!(out.status, DeploymentStatus::Completed);
        assert_ne!(out.status, DeploymentStatus::Running);
        std::fs::remove_dir_all(&dir).ok();
    }
}
