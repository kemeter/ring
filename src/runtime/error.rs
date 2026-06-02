use thiserror::Error;

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("image not found: {0}")]
    ImageNotFound(String),
    #[error("image pull failed: {0}")]
    ImagePullFailed(String),
    #[error("instance creation failed: {0}")]
    InstanceCreationFailed(String),
    // Part of the runtime error contract; not constructed on every code path yet.
    #[allow(dead_code)]
    #[error("config not found: {0}")]
    ConfigNotFound(String),
    #[allow(dead_code)]
    #[error("config key not found: {0}")]
    ConfigKeyNotFound(String),
    #[error("network creation failed: {0}")]
    NetworkCreationFailed(String),
    #[error("stats fetch failed: {0}")]
    StatsFetchFailed(String),
    /// Permanent: VM firmware/kernel file not found at the configured path.
    /// Retrying won't help — the operator must fix `firmware_path`.
    #[error("firmware not found: {0}")]
    FirmwareNotFound(String),
    /// Transient: the VM process failed to start or boot (seccomp kill, KVM
    /// access, transient API socket error). Worth retrying.
    #[error("VM start failed: {0}")]
    VmStartFailed(String),
    /// A published host port is already bound by another process. Matches
    /// the Docker daemon's "port is already allocated" rejection: surface the
    /// conflict to the user immediately instead of booting a VM whose ports
    /// would silently never be reachable.
    #[error("port {0} is already allocated")]
    PortAlreadyInUse(u16),
    /// Permanent (best-effort): the host doesn't have enough free memory to
    /// honour the deployment's requested/limited memory at boot time. Retrying
    /// on the next scheduler tick won't help — the memory isn't coming back on
    /// its own — so this maps to a terminal status rather than a crash loop.
    /// The string carries the actionable "need X, have Y" detail.
    #[error("insufficient resources: {0}")]
    InsufficientResources(String),
    #[error("runtime error: {0}")]
    Other(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
