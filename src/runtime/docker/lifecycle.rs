use super::container::{create_container, remove_container};
use super::instances::list_instances;
use crate::models::config::Config;
use crate::models::deployments::{Deployment, DeploymentStatus, MAX_RESTART_COUNT};
use crate::runtime::error::RuntimeError;
use crate::runtime::types::InstanceStatus;
use bollard::Docker;
use bollard::query_parameters::InspectContainerOptions;
use std::collections::HashMap;
use std::convert::TryInto;

pub(crate) async fn apply(
    mut deployment: Deployment,
    configs: HashMap<String, Config>,
) -> Deployment {
    let docker = match Docker::connect_with_local_defaults() {
        Ok(docker) => docker,
        Err(e) => {
            error!("Failed to connect to Docker: {}", e);
            deployment.status = DeploymentStatus::Error;
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
            DeploymentStatus::ImagePullBackOff,
            "ImagePullBackOff",
            format!("Image '{}' not found", deployment.image),
        ),
        RuntimeError::ImagePullFailed(_) => (
            DeploymentStatus::ImagePullBackOff,
            "ImagePullBackOff",
            format!("Failed to pull image '{}'", deployment.image),
        ),
        RuntimeError::InstanceCreationFailed(msg) => (
            DeploymentStatus::CreateContainerError,
            "InstanceCreationFailed",
            format!("Container creation failed: {}", msg),
        ),
        RuntimeError::NetworkCreationFailed(_) => (
            DeploymentStatus::NetworkError,
            "NetworkCreationFailed",
            format!(
                "Failed to create network for namespace '{}'",
                deployment.namespace
            ),
        ),
        RuntimeError::ConfigNotFound(_) => (
            DeploymentStatus::ConfigError,
            "ConfigError",
            format!("Config not found in namespace '{}'", deployment.namespace),
        ),
        RuntimeError::ConfigKeyNotFound(_) => (
            DeploymentStatus::ConfigError,
            "ConfigError",
            format!(
                "Config key not found in namespace '{}'",
                deployment.namespace
            ),
        ),
        RuntimeError::StatsFetchFailed(msg) => (
            DeploymentStatus::Error,
            "StatsFetchFailed",
            format!("Stats fetch failed: {}", msg),
        ),
        RuntimeError::Other(msg) => (
            DeploymentStatus::Error,
            "RuntimeError",
            format!("Docker error: {}", msg),
        ),
        RuntimeError::Io(e) => (
            DeploymentStatus::FileSystemError,
            "FileSystemError",
            format!("IO error: {}", e),
        ),
        RuntimeError::Json(e) => (
            DeploymentStatus::Error,
            "RuntimeError",
            format!("JSON error: {}", e),
        ),
    };

    error!("[{}] {}: {}", deployment.id, reason, err);
    deployment.status = status;
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
            format!(
                "Deleted {} container(s) for {} marked as deleted",
                instance_count, kind
            ),
            "docker",
            Some("ContainerDeletion"),
        );
    }

    // Clean up temporary config volume files
    let temp_dir = format!("/tmp/ring_configs/{}", deployment.id);
    if std::path::Path::new(&temp_dir).exists() {
        if let Err(e) = std::fs::remove_dir_all(&temp_dir) {
            warn!("Failed to clean up config temp files at {}: {}", temp_dir, e);
        } else {
            debug!("Cleaned up config temp files at {}", temp_dir);
        }
    }

    // Clean up named Docker volumes
    let volumes: Vec<crate::api::dto::deployment::DeploymentVolume> =
        serde_json::from_str(&deployment.volumes).unwrap_or_default();
    for volume in volumes {
        if volume.r#type == "volume" {
            if let Some(name) = volume.source {
                match docker
                    .remove_volume(&name, None::<bollard::query_parameters::RemoveVolumeOptions>)
                    .await
                {
                    Ok(_) => info!("Removed Docker volume '{}'", name),
                    Err(e) => debug!("Failed to remove Docker volume '{}': {}", name, e),
                }
            }
        }
    }
}

async fn handle_job_deployment(
    mut deployment: Deployment,
    docker: Docker,
    configs: HashMap<String, Config>,
) -> Deployment {
    if deployment.status == DeploymentStatus::Deleted {
        debug!("{} marked as deleted. Remove all instances", deployment.id);
        remove_all_instances(&mut deployment, &docker, "job").await;
        return deployment;
    }

    let all_instances = list_instances(&docker, deployment.id.to_string(), "all").await;

    if let Some(instance_id) = all_instances.first() {
        match check_container_status(docker.clone(), instance_id.clone()).await {
            InstanceStatus::Running => {
                deployment.status = DeploymentStatus::Running;
            }
            InstanceStatus::Completed => {
                deployment.status = DeploymentStatus::Completed;
            }
            InstanceStatus::Failed => {
                deployment.status = DeploymentStatus::Failed;
            }
        }
    } else if deployment.status == DeploymentStatus::Creating
        || deployment.status == DeploymentStatus::Pending
    {
        match create_container(&mut deployment, &docker, configs).await {
            Ok(_) => {
                deployment.status = DeploymentStatus::Running;
            }
            Err(err) => {
                handle_create_error(&mut deployment, err, false);
            }
        }
    }

    debug!("Job runtime apply {:?}", deployment.id);
    deployment
}

async fn handle_worker_deployment(
    mut deployment: Deployment,
    docker: Docker,
    configs: HashMap<String, Config>,
) -> Deployment {
    if deployment.status == DeploymentStatus::Deleted {
        debug!("{} marked as deleted. Remove all instances", deployment.id);
        remove_all_instances(&mut deployment, &docker, "worker").await;
    } else if deployment.restart_count >= MAX_RESTART_COUNT {
        deployment.status = DeploymentStatus::CrashLoopBackOff;
        return deployment;
    } else if deployment.status == DeploymentStatus::CrashLoopBackOff {
        return deployment;
    } else {
        let current_count: usize = deployment.instances.len();
        let target_count: usize = match deployment.replicas.try_into() {
            Ok(count) => count,
            Err(_) => {
                error!(
                    "Invalid replicas count for deployment {}: {}",
                    deployment.id, deployment.replicas
                );
                deployment.status = DeploymentStatus::Failed;
                return deployment;
            }
        };

        debug!(
            "Current instances: {}, Target instances: {}",
            current_count, target_count
        );

        match current_count.cmp(&target_count) {
            std::cmp::Ordering::Less => {
                debug!(
                    "Scaling up: {} -> {} (creating 1 container)",
                    current_count, target_count
                );

                match create_container(&mut deployment, &docker, configs).await {
                    Ok(_) => {
                        deployment.emit_event(
                            "info",
                            format!(
                                "Scaled up from {} to {} replicas",
                                current_count,
                                current_count + 1
                            ),
                            "docker",
                            Some("ScaleUp"),
                        );

                        if deployment.status == DeploymentStatus::Pending
                            || deployment.status == DeploymentStatus::Creating
                        {
                            deployment.status = DeploymentStatus::Running;
                        }
                    }
                    Err(err) => {
                        handle_create_error(&mut deployment, err, true);
                    }
                }
            }
            std::cmp::Ordering::Greater => {
                if target_count == 0 {
                    info!(
                        "Scaling deployment {} down to 0: removing container ({} remaining)",
                        deployment.name,
                        current_count - 1
                    );
                } else {
                    debug!(
                        "Scaling down: {} -> {} (removing 1 container)",
                        current_count, target_count
                    );
                }

                if let Some(container_id) = deployment.instances.first().cloned() {
                    remove_container(docker.clone(), container_id.clone()).await;
                    deployment.instances.remove(0);
                    info!(
                        "Container {} removed from deployment {}",
                        container_id, deployment.id
                    );

                    deployment.emit_event(
                        "info",
                        format!(
                            "Scaled down from {} to {} replicas (removed container {})",
                            current_count,
                            current_count - 1,
                            container_id
                        ),
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
    let inspect_options = InspectContainerOptions { size: true };
    match docker
        .inspect_container(&container_id, Some(inspect_options))
        .await
    {
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
