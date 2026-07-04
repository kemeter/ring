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
    #[serde(default)]
    pub(crate) telemetry: TelemetryConfig,
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
    /// Authorize deployments to pull with credentials resolved from the host's
    /// Docker config instead of inlining them. Off by default. A deployment must
    /// *also* set `config.use_host_auth` to activate it: the server authorizes,
    /// the manifest activates.
    #[serde(default)]
    pub(crate) use_host_registry_auth: bool,
    /// Explicit path to the host registry config (Docker `config.json` schema).
    /// When unset, the standard Docker resolution applies (`$DOCKER_CONFIG` then
    /// `~/.docker/config.json`). Set this when the Ring daemon runs as a
    /// different user than the one that ran `docker login`, or to point at a
    /// Podman `containers/auth.json`.
    #[serde(default)]
    pub(crate) host_registry_config: Option<String>,
}

fn default_docker_host() -> String {
    "unix:///var/run/docker.sock".to_string()
}

impl Default for DockerConfig {
    fn default() -> Self {
        DockerConfig {
            enabled: false,
            host: default_docker_host(),
            use_host_registry_auth: false,
            host_registry_config: None,
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
    /// Authorize host-resolved registry credentials. See
    /// [`DockerConfig::use_host_registry_auth`]. Podman's `login` writes to
    /// `containers/auth.json` (same schema as Docker's `config.json`); point at
    /// it with `host_registry_config` when it isn't picked up by the default
    /// Docker resolution.
    #[serde(default)]
    pub(crate) use_host_registry_auth: bool,
    /// Explicit path to the host registry config. See
    /// [`DockerConfig::host_registry_config`].
    #[serde(default)]
    pub(crate) host_registry_config: Option<String>,
}

fn default_podman_host() -> String {
    crate::runtime::podman::resolve_socket_host()
}

impl Default for PodmanConfig {
    fn default() -> Self {
        PodmanConfig {
            enabled: false,
            host: default_podman_host(),
            use_host_registry_auth: false,
            host_registry_config: None,
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
    /// Authorize host-resolved registry credentials. See
    /// [`DockerConfig::use_host_registry_auth`]. containerd has no `login` of
    /// its own; tools like `nerdctl` write to `~/.docker/config.json`, which is
    /// the default this resolves.
    #[serde(default)]
    pub(crate) use_host_registry_auth: bool,
    /// Explicit path to the host registry config. See
    /// [`DockerConfig::host_registry_config`].
    #[serde(default)]
    pub(crate) host_registry_config: Option<String>,
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
            use_host_registry_auth: false,
            host_registry_config: None,
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

/// `[server.telemetry]` — OpenTelemetry export. One sub-block per signal so the
/// table can grow without breaking existing configs: `traces` ships now; `logs`
/// and `metrics` are reserved for later phases and are intentionally absent from
/// the struct until implemented (an unknown `[server.telemetry.logs]` table is
/// simply ignored by serde today, so adding it later is backward-compatible).
#[derive(Deserialize, Debug, Clone, Default)]
pub(crate) struct TelemetryConfig {
    #[serde(default)]
    pub(crate) traces: TracesConfig,
}

/// `[server.telemetry.traces]` — OTLP/gRPC span export. Opt-in: with `enabled`
/// false (the default) no exporter is built and Ring runs exactly as before.
///
/// Standard `OTEL_*` environment variables override the TOML values so a
/// deployment can point Ring at its collector without editing the file:
/// `OTEL_EXPORTER_OTLP_ENDPOINT` overrides `endpoint`, `OTEL_SERVICE_NAME`
/// overrides `service_name`. Resolution order is env > TOML > built-in default.
#[derive(Deserialize, Debug, Clone)]
pub(crate) struct TracesConfig {
    #[serde(default)]
    pub(crate) enabled: bool,
    /// OTLP/gRPC collector endpoint, e.g. `http://collector:4317`.
    #[serde(default = "default_traces_endpoint")]
    pub(crate) endpoint: String,
    /// `service.name` resource attribute reported to the collector.
    #[serde(default = "default_traces_service_name")]
    pub(crate) service_name: String,
    /// Sampler: `parent_based_always_on` (default) follows an upstream decision
    /// and samples roots; `always_on`, `always_off`, or `ratio:<0..1>` (e.g.
    /// `ratio:0.1` for 10%, wrapped in parent-based).
    #[serde(default = "default_traces_sampler")]
    pub(crate) sampler: String,
}

fn default_traces_endpoint() -> String {
    "http://127.0.0.1:4317".to_string()
}

fn default_traces_service_name() -> String {
    "ring-server".to_string()
}

fn default_traces_sampler() -> String {
    "parent_based_always_on".to_string()
}

impl Default for TracesConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            endpoint: default_traces_endpoint(),
            service_name: default_traces_service_name(),
            sampler: default_traces_sampler(),
        }
    }
}

impl TracesConfig {
    /// Effective collector endpoint: `OTEL_EXPORTER_OTLP_ENDPOINT` (or the
    /// traces-specific `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`, which wins per the
    /// OTLP spec) overrides the configured value.
    pub(crate) fn resolved_endpoint(&self) -> String {
        std::env::var("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT")
            .or_else(|_| std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT"))
            .unwrap_or_else(|_| self.endpoint.clone())
    }

    /// Effective `service.name`: `OTEL_SERVICE_NAME` overrides the configured
    /// value.
    pub(crate) fn resolved_service_name(&self) -> String {
        std::env::var("OTEL_SERVICE_NAME").unwrap_or_else(|_| self.service_name.clone())
    }
}

#[cfg(test)]
mod telemetry_tests {
    use super::*;

    #[test]
    fn traces_disabled_by_default() {
        let c = TracesConfig::default();
        assert!(!c.enabled);
        assert_eq!(c.endpoint, "http://127.0.0.1:4317");
        assert_eq!(c.service_name, "ring-server");
        assert_eq!(c.sampler, "parent_based_always_on");
    }

    #[test]
    fn telemetry_absent_from_toml_yields_disabled_traces() {
        // A `[server]` table with no telemetry block must not enable anything.
        let cfg: ServerConfig = toml::from_str("").unwrap();
        assert!(!cfg.telemetry.traces.enabled);
    }

    #[test]
    fn traces_block_parses_from_toml() {
        let cfg: ServerConfig = toml::from_str(
            r#"
            [telemetry.traces]
            enabled = true
            endpoint = "http://collector:4317"
            service_name = "ring-prod"
            sampler = "ratio:0.25"
            "#,
        )
        .unwrap();
        assert!(cfg.telemetry.traces.enabled);
        assert_eq!(cfg.telemetry.traces.endpoint, "http://collector:4317");
        assert_eq!(cfg.telemetry.traces.service_name, "ring-prod");
        assert_eq!(cfg.telemetry.traces.sampler, "ratio:0.25");
    }
}
