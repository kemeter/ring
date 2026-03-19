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
    #[error("runtime error: {0}")]
    Other(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
