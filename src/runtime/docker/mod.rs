mod container;
mod health_check;
mod instances;
mod lifecycle;
mod logs;

use bollard::Docker;
use crate::runtime::error::RuntimeError;

pub(crate) use container::remove_container_by_id;
pub(crate) use health_check::execute_health_check_for_instance;
pub(crate) use instances::{list_instances, list_instances_with_names};
pub(crate) use lifecycle::apply;
pub(crate) use logs::{logs, logs_stream};

pub(crate) struct DockerImage {
    pub name: String,
    pub tag: String,
    pub auth: Option<(String, String, String)>,
}

impl From<bollard::errors::Error> for RuntimeError {
    fn from(err: bollard::errors::Error) -> Self {
        let err_msg = err.to_string();
        if err_msg.contains("404") || err_msg.contains("not found") || err_msg.contains("manifest unknown") {
            RuntimeError::ImageNotFound(err_msg)
        } else {
            RuntimeError::Other(err_msg)
        }
    }
}

pub(crate) fn connect() -> Result<Docker, RuntimeError> {
    Docker::connect_with_local_defaults()
        .map_err(|e| RuntimeError::Other(format!("Failed to connect to Docker: {}", e)))
}

pub(crate) fn tiny_id() -> String {
    use rand::Rng;
    let mut rng = rand::rng();
    format!("{:08x}", rng.random::<u32>())
}
