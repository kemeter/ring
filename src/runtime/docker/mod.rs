mod container;
pub(crate) mod docker_lifecycle;
mod health_check;
mod instances;
mod lifecycle;
mod logs;
mod stats;

use crate::runtime::error::RuntimeError;
use bollard::Docker;

pub(crate) use container::remove_container_by_id;
pub(crate) use health_check::execute_health_check_for_instance;
pub(crate) use instances::{list_instances, list_instances_with_names};
pub(crate) use lifecycle::apply;
pub(crate) use logs::{logs, logs_stream};
pub(crate) use stats::{
    compute_cpu_percent, compute_disk_io_stats, compute_memory_stats, compute_network_stats,
    compute_pid_stats, fetch_container_stats, fetch_restart_count,
};

pub(crate) struct DockerImage {
    pub name: String,
    pub tag: String,
    pub auth: Option<(String, String, String)>,
}

impl From<bollard::errors::Error> for RuntimeError {
    fn from(err: bollard::errors::Error) -> Self {
        let err_msg = err.to_string();
        if err_msg.contains("404")
            || err_msg.contains("not found")
            || err_msg.contains("manifest unknown")
        {
            RuntimeError::ImageNotFound(err_msg)
        } else {
            RuntimeError::Other(err_msg)
        }
    }
}

pub(crate) fn connect() -> Result<Docker, RuntimeError> {
    let host = std::env::var("DOCKER_HOST")
        .unwrap_or_else(|_| "unix:///var/run/docker.sock".to_string());

    connect_with_host(&host)
}

pub(crate) fn connect_with_host(host: &str) -> Result<Docker, RuntimeError> {
    if host.starts_with("unix://") {
        let socket_path = host.trim_start_matches("unix://");
        Docker::connect_with_socket(socket_path, 120, bollard::API_DEFAULT_VERSION)
            .map_err(|e| RuntimeError::Other(format!("Failed to connect to Docker socket {}: {}", host, e)))
    } else if host.starts_with("tcp://") {
        Docker::connect_with_http(host, 120, bollard::API_DEFAULT_VERSION)
            .map_err(|e| RuntimeError::Other(format!("Failed to connect to Docker at {}: {}", host, e)))
    } else {
        Docker::connect_with_local_defaults()
            .map_err(|e| RuntimeError::Other(format!("Failed to connect to Docker: {}", e)))
    }
}

pub(crate) fn tiny_id() -> String {
    use rand::Rng;
    let mut rng = rand::rng();
    format!("{:08x}", rng.random::<u32>())
}
