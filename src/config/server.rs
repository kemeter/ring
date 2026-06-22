//! Daemon-side configuration: everything the Ring *server* does, as opposed to
//! how a CLI *reaches* a server (that's the per-context client config in
//! [`crate::config::config`]). Parsed from the top-level `[server]` table of
//! `config.toml`, which lives outside `[contexts.*]` — a context describes one
//! client→server connection and has no business deciding which runtimes that
//! server enables.
//!
//! The split (client `[contexts.*]` vs daemon `[server]`) mirrors Nomad's
//! `client {}` / `server {}` stanzas: one tool, both roles, one file.

use serde::Deserialize;

/// Top-level `[server]` table. Shared by the whole file (a host runs one daemon,
/// whatever client contexts point at it).
#[derive(Deserialize, Debug, Clone, Default)]
pub(crate) struct ServerConfig {
    #[serde(default)]
    pub(crate) scheduler: Scheduler,
    #[serde(default)]
    pub(crate) runtime: RuntimesConfig,
    #[serde(default)]
    pub(crate) dashboard: DashboardConfig,
}

#[derive(Deserialize, Debug, Clone)]
pub(crate) struct Scheduler {
    #[serde(default = "default_scheduler_interval")]
    pub(crate) interval: u64,
}

fn default_scheduler_interval() -> u64 {
    10
}

impl Default for Scheduler {
    fn default() -> Self {
        Scheduler { interval: 10 }
    }
}

/// Container runtimes. All opt-in: a runtime is only registered when its
/// `enabled` flag is `true`. See `commands::server` for the opt-in + fail-fast
/// registration logic.
#[derive(Deserialize, Debug, Clone, Default)]
pub(crate) struct RuntimesConfig {
    #[serde(default)]
    pub(crate) docker: DockerConfig,
    #[serde(default)]
    pub(crate) podman: PodmanConfig,
    #[serde(default)]
    pub(crate) containerd: ContainerdConfig,
    #[serde(default)]
    pub(crate) cloud_hypervisor: CloudHypervisorConfig,
    #[serde(default)]
    pub(crate) firecracker: FirecrackerConfig,
}

#[derive(Deserialize, Debug, Clone)]
pub(crate) struct DockerConfig {
    /// Whether to register the Docker runtime. Off by default: runtimes are
    /// opt-in. When `true` and the daemon doesn't answer at startup, Ring fails
    /// fast (a requested-but-unreachable runtime is a config error).
    #[serde(default)]
    pub(crate) enabled: bool,
    /// Docker host URL. Examples:
    /// - "unix:///var/run/docker.sock" (default)
    /// - "tcp://192.168.1.100:2375"
    /// - "tcp://192.168.1.100:2376" (with TLS)
    #[serde(default = "default_docker_host")]
    pub(crate) host: String,
}

fn default_docker_host() -> String {
    "unix:///var/run/docker.sock".to_string()
}

impl Default for DockerConfig {
    fn default() -> Self {
        DockerConfig {
            enabled: false,
            host: default_docker_host(),
        }
    }
}

#[derive(Deserialize, Debug, Clone)]
pub(crate) struct PodmanConfig {
    /// Whether to register the Podman runtime. Off by default (runtimes are
    /// opt-in). Podman exposes a Docker-compatible API via `podman system
    /// service`, so Ring drives it with the same `bollard` client. When `true`
    /// and the socket doesn't answer at startup, Ring fails fast.
    #[serde(default)]
    pub(crate) enabled: bool,
    /// Podman API socket. Defaults to the rootless-first resolution
    /// (`RING_PODMAN_HOST` → `DOCKER_HOST` → `unix:///run/user/$UID/podman/podman.sock`
    /// → `unix:///run/podman/podman.sock`). Override here to pin a specific socket.
    #[serde(default = "default_podman_host")]
    pub(crate) host: String,
}

fn default_podman_host() -> String {
    crate::runtime::podman::resolve_socket_host()
}

impl Default for PodmanConfig {
    fn default() -> Self {
        PodmanConfig {
            enabled: false,
            host: default_podman_host(),
        }
    }
}

#[derive(Deserialize, Debug, Clone)]
pub(crate) struct ContainerdConfig {
    /// Whether to register the containerd runtime. Off by default (runtimes
    /// are opt-in). Unlike Podman, containerd speaks its own native gRPC API,
    /// so Ring drives it directly with no Docker daemon in between. When `true`
    /// and the socket doesn't answer at startup, Ring fails fast.
    #[serde(default)]
    pub(crate) enabled: bool,
    /// Path to the containerd gRPC Unix socket. Defaults to the stock location
    /// used by `containerd`, k3s and RKE2.
    #[serde(default = "default_containerd_socket")]
    pub(crate) socket: String,
    /// containerd metadata namespace under which Ring creates its images,
    /// snapshots, containers and tasks. This is containerd's own partition
    /// concept (akin to `k8s.io`, `moby`, `default`) and is unrelated to a Ring
    /// deployment namespace — keeping Ring's objects under their own namespace
    /// avoids colliding with Kubernetes or Docker on a shared host.
    #[serde(default = "default_containerd_namespace")]
    pub(crate) namespace: String,
}

fn default_containerd_socket() -> String {
    "/run/containerd/containerd.sock".to_string()
}

fn default_containerd_namespace() -> String {
    "ring".to_string()
}

impl Default for ContainerdConfig {
    fn default() -> Self {
        ContainerdConfig {
            enabled: false,
            socket: default_containerd_socket(),
            namespace: default_containerd_namespace(),
        }
    }
}

/// User-facing configuration for the Cloud Hypervisor runtime. Parsed from the
/// `[server.runtime.cloud_hypervisor]` section of `config.toml`.
///
/// All fields are optional; when unset, `CloudHypervisorRuntimeConfig::default`
/// falls back to `$RING_CONFIG_DIR/cloud-hypervisor/...`.
#[derive(Deserialize, Debug, Clone, Default)]
pub(crate) struct CloudHypervisorConfig {
    /// Whether to register the Cloud Hypervisor runtime. Off by default
    /// (runtimes are opt-in). When `true` and the `cloud-hypervisor` binary
    /// can't be resolved at startup, Ring fails fast.
    #[serde(default)]
    pub(crate) enabled: bool,
    pub(crate) binary_path: Option<String>,
    pub(crate) firmware_path: Option<String>,
    pub(crate) socket_dir: Option<String>,
    /// Forwarded to `cloud-hypervisor --seccomp <value>`. Accepts `true`
    /// (default), `false` or `log`. Set to `false` on hosts where the kernel
    /// uses syscalls not whitelisted by CH (otherwise VMs die with SIGSYS).
    pub(crate) seccomp: Option<String>,
    /// Maximum size (bytes) for a per-VM console log before rotation kicks
    /// in. Defaults to 10 MiB. Set to 0 to disable rotation entirely.
    pub(crate) max_console_log_bytes: Option<u64>,
    /// How many rotated console log backups to keep alongside the live file
    /// (`.console.log.1`, `.console.log.2`, ...). Defaults to 3.
    pub(crate) max_console_log_backups: Option<u32>,
}

/// User-facing configuration for the Firecracker runtime. Parsed from the
/// `[server.runtime.firecracker]` section of `config.toml`.
///
/// All fields are optional; when unset, `FirecrackerRuntimeConfig::default`
/// falls back to `$RING_CONFIG_DIR/firecracker/...`.
#[derive(Deserialize, Debug, Clone, Default)]
pub(crate) struct FirecrackerConfig {
    /// Whether to register the Firecracker runtime. Off by default (runtimes
    /// are opt-in). When `true` and the `firecracker` binary can't be resolved
    /// at startup, Ring fails fast.
    #[serde(default)]
    pub(crate) enabled: bool,
    pub(crate) binary_path: Option<String>,
    /// Path to the uncompressed kernel image (`vmlinux`). Firecracker boots a
    /// kernel directly — there is no firmware step like Cloud Hypervisor.
    pub(crate) kernel_path: Option<String>,
    pub(crate) socket_dir: Option<String>,
    /// Kernel command line passed to every microVM.
    pub(crate) boot_args: Option<String>,
    /// Maximum size (bytes) for a per-VM console log before rotation kicks
    /// in. Defaults to 10 MiB. Set to 0 to disable rotation entirely.
    pub(crate) max_console_log_bytes: Option<u64>,
    /// How many rotated console log backups to keep alongside the live file
    /// (`.console.log.1`, `.console.log.2`, ...). Defaults to 3.
    pub(crate) max_console_log_backups: Option<u32>,
}

/// User-facing configuration for the embedded web dashboard. Off by default
/// to keep the server surface minimal until an operator opts in.
#[derive(Deserialize, Debug, Clone)]
pub(crate) struct DashboardConfig {
    /// When true, `ring server start` spawns the dashboard on
    /// `listen_address`. When false (the default), the dashboard is not
    /// served by this Ring instance — operators can still run
    /// `ring dashboard` locally against any API.
    #[serde(default)]
    pub(crate) enabled: bool,
    /// `host:port` for the dashboard to bind to. Distinct from the API
    /// port to keep concerns separated.
    #[serde(default = "default_dashboard_listen_address")]
    pub(crate) listen_address: String,
}

fn default_dashboard_listen_address() -> String {
    "127.0.0.1:3031".to_string()
}

impl Default for DashboardConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            listen_address: default_dashboard_listen_address(),
        }
    }
}
