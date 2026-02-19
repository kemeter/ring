use bollard::Docker;
use bollard::query_parameters::InspectContainerOptions;
use std::collections::HashMap;
use std::convert::TryInto;
use crate::models::deployments::Deployment;
use crate::models::config::Config;
use crate::runtime::error::RuntimeError;
use crate::runtime::types::InstanceStatus;
use super::container::{create_container, remove_container};
use super::instances::list_instances;

pub(crate) async fn apply(mut deployment: Deployment, configs: HashMap<String, Config>) -> Deployment {
    let docker = match Docker::connect_with_local_defaults() {
        Ok(docker) => docker,
        Err(e) => {
            error!("Failed to connect to Docker: {}", e);
            deployment.status = "Error".to_string();
            return deployment;
        }
    };

    deployment.instances = list_instances(&docker, deployment.id.to_string(), "active").await;

    if deployment.kind == "job" {
        handle_job_deployment(deployment, docker, configs).await
    } else {
        handle_worker_deployment(deployment, docker, configs).await
    }
}

fn handle_create_error(deployment: &mut Deployment, err: RuntimeError, increment_restart: bool) {
    if increment_restart {
        deployment.restart_count += 1;
    }

    let (status, reason, message) = match &err {
        RuntimeError::ImageNotFound(_) => (
            "ImagePullBackOff", "ImagePullBackOff",
            format!("Image '{}' not found", deployment.image),
        ),
        RuntimeError::ImagePullFailed(_) => (
            "ImagePullBackOff", "ImagePullBackOff",
            format!("Failed to pull image '{}'", deployment.image),
        ),
        RuntimeError::InstanceCreationFailed(msg) => (
            "CreateContainerError", "InstanceCreationFailed",
            format!("Container creation failed: {}", msg),
        ),
        RuntimeError::NetworkCreationFailed(_) => (
            "NetworkError", "NetworkCreationFailed",
            format!("Failed to create network for namespace '{}'", deployment.namespace),
        ),
        RuntimeError::ConfigNotFound(_) => (
            "ConfigError", "ConfigError",
            format!("Config not found in namespace '{}'", deployment.namespace),
        ),
        RuntimeError::ConfigKeyNotFound(_) => (
            "ConfigError", "ConfigError",
            format!("Config key not found in namespace '{}'", deployment.namespace),
        ),
        RuntimeError::FileSystemError(_) => (
            "FileSystemError", "FileSystemError",
            "Failed to access file system for config mount".to_string(),
        ),
        RuntimeError::ConnectionFailed(msg) => (
            "Error", "ConnectionFailed",
            format!("Connection failed: {}", msg),
        ),
        RuntimeError::Other(msg) => (
            "Error", "RuntimeError",
            format!("Docker error: {}", msg),
        ),
    };

    error!("[{}] {}: {}", deployment.id, reason, err);
    deployment.status = status.to_string();
    deployment.emit_event("error", message, "docker", Some(reason));
}

async fn remove_all_instances(deployment: &mut Deployment, docker: &Docker, kind: &str) {
    let instance_count = deployment.instances.len();
    for instance in deployment.instances.iter() {
        remove_container(docker.clone(), instance.to_string()).await;
        info!("Docker container {} deleted", instance);
    }

    if instance_count > 0 {
        deployment.emit_event(
            "info",
            format!("Deleted {} container(s) for {} marked as deleted", instance_count, kind),
            "docker",
            Some("ContainerDeletion"),
        );
    }
}

async fn handle_job_deployment(mut deployment: Deployment, docker: Docker, configs: HashMap<String, Config>) -> Deployment {
    if deployment.status == "deleted" {
        debug!("{} marked as deleted. Remove all instances", deployment.id);
        remove_all_instances(&mut deployment, &docker, "job").await;
        return deployment;
    }

    let all_instances = list_instances(&docker, deployment.id.to_string(), "all").await;

    if !all_instances.is_empty() {
        for instance_id in &all_instances {
            match check_container_status(docker.clone(), instance_id.clone()).await {
                InstanceStatus::Running => {
                    deployment.status = "running".to_string();
                    break;
                }
                InstanceStatus::Completed => {
                    deployment.status = "completed".to_string();
                    break;
                }
                InstanceStatus::Failed => {
                    deployment.status = "failed".to_string();
                    break;
                }
            }
        }
    } else if deployment.status == "creating" || deployment.status == "pending" {
        match create_container(&mut deployment, &docker, configs).await {
            Ok(_) => {
                deployment.status = "running".to_string();
            }
            Err(err) => {
                handle_create_error(&mut deployment, err, false);
            }
        }
    }

    debug!("Job runtime apply {:?}", deployment.id);
    deployment
}

async fn handle_worker_deployment(mut deployment: Deployment, docker: Docker, configs: HashMap<String, Config>) -> Deployment {
    if deployment.restart_count >= 5 && deployment.status != "deleted" {
        deployment.status = "CrashLoopBackOff".to_string();
        return deployment;
    }

    if deployment.status == "CrashLoopBackOff" {
        return deployment;
    }

    if deployment.status == "deleted" {
        debug!("{} marked as deleted. Remove all instances", deployment.id);
        remove_all_instances(&mut deployment, &docker, "worker").await;
    } else {
        let current_count: usize = deployment.instances.len();
        let target_count: usize = match deployment.replicas.try_into() {
            Ok(count) => count,
            Err(_) => {
                error!("Invalid replicas count for deployment {}: {}", deployment.id, deployment.replicas);
                deployment.status = "Failed".to_string();
                return deployment;
            }
        };

        debug!("Current instances: {}, Target instances: {}", current_count, target_count);

        match current_count.cmp(&target_count) {
            std::cmp::Ordering::Less => {
                debug!("Scaling up: {} -> {} (creating 1 container)", current_count, target_count);

                match create_container(&mut deployment, &docker, configs).await {
                    Ok(_) => {
                        deployment.emit_event(
                            "info",
                            format!("Scaled up from {} to {} replicas", current_count, current_count + 1),
                            "docker",
                            Some("ScaleUp"),
                        );

                        if deployment.status == "pending" || deployment.status == "creating" {
                            deployment.status = "running".to_string();
                        }
                    }
                    Err(err) => {
                        handle_create_error(&mut deployment, err, true);
                    }
                }
            }
            std::cmp::Ordering::Greater => {
                if target_count == 0 {
                    info!("Scaling deployment {} down to 0: removing container ({} remaining)",
                          deployment.name, current_count - 1);
                } else {
                    debug!("Scaling down: {} -> {} (removing 1 container)", current_count, target_count);
                }

                if let Some(container_id) = deployment.instances.first().cloned() {
                    remove_container(docker.clone(), container_id.clone()).await;
                    deployment.instances.remove(0);
                    info!("Container {} removed from deployment {}", container_id, deployment.id);

                    deployment.emit_event(
                        "info",
                        format!("Scaled down from {} to {} replicas (removed container {})", current_count, current_count - 1, container_id),
                        "docker",
                        Some("ScaleDown"),
                    );
                }
            }
            std::cmp::Ordering::Equal => {
                debug!("Replicas count matches target: {} instances", current_count);
            }
        }
    }

    debug!("Worker runtime apply {:?}", deployment.id);
    deployment
}

async fn check_container_status(docker: Docker, container_id: String) -> InstanceStatus {
    let inspect_options = InspectContainerOptions {
        size: true,
        ..Default::default()
    };
    match docker.inspect_container(&container_id, Some(inspect_options)).await {
        Ok(info) => {
            if let Some(state) = info.state {
                if state.running == Some(true) {
                    InstanceStatus::Running
                } else if state.exit_code == Some(0) {
                    InstanceStatus::Completed
                } else {
                    InstanceStatus::Failed
                }
            } else {
                InstanceStatus::Failed
            }
        }
        Err(e) => {
            debug!("Failed to inspect container {}: {}", container_id, e);
            InstanceStatus::Failed
        }
    }
}
