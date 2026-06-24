//! Connection management for the containerd runtime.
//!
//! [`ContainerdRuntimeConfig`] is the resolved, daemon-side configuration (which
//! socket, which containerd namespace). [`ContainerdLifecycle`] holds it and a
//! lazily-created gRPC [`Client`]; the trait impl lives in
//! [`super::lifecycle`].

use crate::config::server::ContainerdConfig;
use crate::hypervisor::error::RuntimeError;
use crate::runtime::registry_auth::HostAuthSettings;
use containerd_client::Client;
use containerd_client::services::v1::version_client::VersionClient;

/// Default snapshotter. `overlayfs` is the stock default on virtually every
/// containerd install (Docker, k3s, RKE2 all use it). Pinning it here keeps the
/// rootfs path consistent with what the image was unpacked into during pull.
pub(crate) const DEFAULT_SNAPSHOTTER: &str = "overlayfs";

/// containerd's built-in runtime shim name (runc v2). This is the `runtime.name`
/// field on the container object — not a binary path.
pub(crate) const DEFAULT_RUNTIME: &str = "io.containerd.runc.v2";

/// Resolved configuration for the containerd runtime.
#[derive(Clone, Debug)]
pub(crate) struct ContainerdRuntimeConfig {
    /// Path to the containerd gRPC Unix socket.
    pub(crate) socket: String,
    /// containerd metadata namespace Ring operates under.
    pub(crate) namespace: String,
    /// Server-side host registry auth settings for this runtime.
    pub(crate) host_auth: HostAuthSettings,
}

impl ContainerdRuntimeConfig {
    /// Build the runtime config from the user-facing `[server.runtime.containerd]`
    /// section. Both fields already carry sensible defaults from the config
    /// layer, so this is a straight copy.
    pub(crate) fn from_user_config(cfg: &ContainerdConfig) -> Self {
        Self {
            socket: cfg.socket.clone(),
            namespace: cfg.namespace.clone(),
            host_auth: HostAuthSettings {
                authorized: cfg.use_host_registry_auth,
                config_path: cfg.host_registry_config.clone(),
            },
        }
    }
}

/// The containerd runtime handle. Cloning is cheap: it only carries the resolved
/// config; each gRPC call opens its own multiplexed stream over a fresh client
/// built from the socket (containerd's gRPC channel is connection-pooled by
/// tonic under the hood).
#[derive(Clone)]
pub(crate) struct ContainerdLifecycle {
    pub(crate) config: ContainerdRuntimeConfig,
}

impl ContainerdLifecycle {
    pub(crate) fn new(config: ContainerdRuntimeConfig) -> Self {
        Self { config }
    }

    /// Open a gRPC client to the configured socket.
    ///
    /// `Client::from_path` actually dials the Unix socket, so unlike Docker's
    /// lazy `connect`, a failure here already means containerd is unreachable.
    pub(crate) async fn connect(&self) -> Result<Client, RuntimeError> {
        Client::from_path(&self.config.socket).await.map_err(|e| {
            RuntimeError::Other(format!(
                "failed to connect to containerd socket {}: {}",
                self.config.socket, e
            ))
        })
    }

    /// Fail-fast availability probe used at daemon startup, mirroring the Docker
    /// runtime's `connect_and_verify`. A successful `Version` round-trip proves
    /// the socket is up *and* speaking the containerd API, so it gates whether
    /// the runtime is registered at all.
    pub(crate) async fn connect_and_verify(
        config: ContainerdRuntimeConfig,
    ) -> Result<Self, RuntimeError> {
        let lifecycle = Self::new(config);
        let client = lifecycle.connect().await?;
        let mut version = VersionClient::new(client.channel());
        let resp = version.version(()).await.map_err(|e| {
            RuntimeError::Other(format!(
                "containerd at {} did not answer Version: {}",
                lifecycle.config.socket, e
            ))
        })?;
        let v = resp.into_inner();
        info!(
            "containerd runtime ready (version {} / {}, namespace '{}')",
            v.version, v.revision, lifecycle.config.namespace
        );
        Ok(lifecycle)
    }
}
