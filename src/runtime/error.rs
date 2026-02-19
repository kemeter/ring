use std::fmt;

#[derive(Debug)]
pub enum RuntimeError {
    ImageNotFound(String),
    ImagePullFailed(String),
    InstanceCreationFailed(String),
    ConfigNotFound(String),
    ConfigKeyNotFound(String),
    FileSystemError(String),
    NetworkCreationFailed(String),
    Other(String),
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RuntimeError::ImageNotFound(msg) => write!(f, "Image not found: {}", msg),
            RuntimeError::ImagePullFailed(msg) => write!(f, "Image pull failed: {}", msg),
            RuntimeError::InstanceCreationFailed(msg) => {
                write!(f, "Instance creation failed: {}", msg)
            }
            RuntimeError::ConfigNotFound(msg) => write!(f, "Config not found: {}", msg),
            RuntimeError::ConfigKeyNotFound(msg) => write!(f, "Config key not found: {}", msg),
            RuntimeError::FileSystemError(msg) => write!(f, "File system error: {}", msg),
            RuntimeError::NetworkCreationFailed(msg) => write!(f, "Network creation failed: {}", msg),
            RuntimeError::Other(msg) => write!(f, "Runtime error: {}", msg),
        }
    }
}

impl std::error::Error for RuntimeError {}

impl From<std::io::Error> for RuntimeError {
    fn from(err: std::io::Error) -> Self {
        RuntimeError::FileSystemError(format!("{}", err))
    }
}

impl From<serde_json::Error> for RuntimeError {
    fn from(err: serde_json::Error) -> Self {
        RuntimeError::Other(format!("JSON parsing error: {}", err))
    }
}
