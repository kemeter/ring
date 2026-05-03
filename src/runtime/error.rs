use thiserror::Error;

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("image not found: {0}")]
    ImageNotFound(String),
    #[error("image pull failed: {0}")]
    ImagePullFailed(String),
    #[error("instance creation failed: {0}")]
    InstanceCreationFailed(String),
    #[error("config not found: {0}")]
    ConfigNotFound(String),
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
    #[error("runtime error: {0}")]
    Other(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
