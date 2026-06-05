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
    pub(crate) cloud_hypervisor: CloudHypervisorConfig,
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
