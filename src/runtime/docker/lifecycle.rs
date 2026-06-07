use super::container::{create_container, remove_container};
use super::instances::list_instances;
use crate::hypervisor::error::RuntimeError;
use crate::hypervisor::types::InstanceStatus;
use crate::models::deployments::{Deployment, DeploymentStatus, MAX_RESTART_COUNT};
use crate::models::volume::ResolvedMount;
use crate::scheduler::intentional_shutdowns::IntentionalShutdowns;
use bollard::Docker;
use bollard::query_parameters::InspectContainerOptions;
use std::cmp::Ordering;
use std::convert::TryInto;

pub(crate) async fn apply(
    mut deployment: Deployment,
    docker: Docker,
    resolved_mounts: Vec<ResolvedMount>,
    intentional_shutdowns: IntentionalShutdowns,
) -> Deployment {
    let status_filter = if deployment.status == DeploymentStatus::Deleted {
        "all"
    } else {
        "active"
    };
    deployment.instances = list_instances(&docker, deployment.id.to_string(), status_filter).await;

    if deployment.kind == "job" {
        handle_job_deployment(deployment, docker, resolved_mounts, intentional_shutdowns).await
    } else {
        handle_worker_deployment(deployment, docker, resolved_mounts, intentional_shutdowns).await
    }
}

fn handle_create_error(deployment: &mut Deployment, err: RuntimeError, increment_restart: bool) {
    if increment_restart {
        deployment.restart_count += 1;
    }

    let (status, reason, message) = match &err {
        RuntimeError::ImageNotFound(detail) => (
            DeploymentStatus::ImagePullBackOff,
            "image_pull_back_off",
            // Preserve the inner detail — it carries operator-relevant
            // context like `image_pull_policy=Never forbids pulling` that a
            // generic "not found" would drop.
            format!("Image '{}' not found: {}", deployment.image, detail),
        ),
        RuntimeError::ImagePullFailed(detail) => (
            DeploymentStatus::ImagePullBackOff,
            "image_pull_back_off",
            format!("Failed to pull image '{}': {}", deployment.image, detail),
        ),
        RuntimeError::InstanceCreationFailed(msg) => (
            DeploymentStatus::CreateContainerError,
            "instance_creation_failed",
            format!("Container creation failed: {}", msg),
        ),
        RuntimeError::NetworkCreationFailed(_) => (
            DeploymentStatus::NetworkError,
            "network_creation_failed",
            format!(
                "Failed to create network for namespace '{}'",
                deployment.namespace
            ),
        ),
        RuntimeError::ConfigNotFound(_) => (
            DeploymentStatus::ConfigError,
            "config_error",
            format!("Config not found in namespace '{}'", deployment.namespace),
        ),
        RuntimeError::ConfigKeyNotFound(_) => (
            DeploymentStatus::ConfigError,
            "config_error",
            format!(
                "Config key not found in namespace '{}'",
                deployment.namespace
            ),
        ),
        RuntimeError::StatsFetchFailed(msg) => (
            DeploymentStatus::Error,
            "stats_fetch_failed",
            format!("Stats fetch failed: {}", msg),
        ),
        RuntimeError::InsufficientResources(detail) => (
            DeploymentStatus::InsufficientResources,
            "insufficient_resources",
            detail.clone(),
        ),
        RuntimeError::Other(msg) => (
            DeploymentStatus::Error,
            "runtime_error",
            format!("Docker error: {}", msg),
        ),
        // CH-specific variants — Docker should never produce them, but the
        // enum is shared so we map them to the closest Docker-side state.
        RuntimeError::FirmwareNotFound(msg) => (
            DeploymentStatus::Failed,
            "firmware_not_found",
            format!("Firmware not found: {}", msg),
        ),
        RuntimeError::VmStartFailed(msg) => (
            DeploymentStatus::Error,
            "vm_start_failed",
            format!("VM start failed: {}", msg),
        ),
        // The CH runtime emits this; the Docker runtime doesn't (the daemon
        // rejects port conflicts itself through InstanceCreationFailed). The
        // arm exists to keep the match exhaustive over the shared enum.
        RuntimeError::PortAlreadyInUse(port) => (
            DeploymentStatus::Error,
            "port_allocation_failed",
            format!("Port {} is already allocated", port),
        ),
        RuntimeError::Io(e) => (
            DeploymentStatus::FileSystemError,
            "file_system_error",
            format!("IO error: {}", e),
        ),
        RuntimeError::Json(e) => (
            DeploymentStatus::Error,
            "runtime_error",
            format!("JSON error: {}", e),
        ),
    };

    error!("[{}] {}: {}", deployment.id, reason, err);
    deployment.status = status;
    deployment.emit_event("error", message, "docker", Some(reason));
}

async fn remove_all_instances(
    deployment: &mut Deployment,
    docker: &Docker,
    kind: &str,
    intentional_shutdowns: &IntentionalShutdowns,
) {
    let instance_count = deployment.instances.len();
    for instance in deployment.instances.iter() {
        intentional_shutdowns.mark(instance.to_string()).await;
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
            Some("container_deletion"),
        );
    }

    // Clean up temporary config volume files
    let temp_dir = format!("/tmp/ring_configs/{}", deployment.id);
    if std::path::Path::new(&temp_dir).exists() {
        if let Err(e) = std::fs::remove_dir_all(&temp_dir) {
            warn!(
                "Failed to clean up config temp files at {}: {}",
                temp_dir, e
            );
        } else {
            debug!("Cleaned up config temp files at {}", temp_dir);
        }
    }

    // Named Docker volumes are intentionally preserved across deployment deletions.
    // A volume's lifecycle is independent of any single deployment: deleting one
    // deployment must never destroy data that other deployments (or future
    // redeployments under the same name) may rely on. Volume removal is an
    // explicit operation, not a side effect of deployment cleanup.
    //
    // Anonymous volumes (auto-created from an image's `VOLUME` directive) are a
    // different story: they carry no name and no data the operator asked to
    // keep, so they are reaped per-container via the `v(true)` flag in
    // `remove_container` to avoid orphan accumulation.
}

async fn handle_job_deployment(
    mut deployment: Deployment,
    docker: Docker,
    resolved_mounts: Vec<ResolvedMount>,
    intentional_shutdowns: IntentionalShutdowns,
) -> Deployment {
    if deployment.status == DeploymentStatus::Deleted {
        debug!("{} marked as deleted. Remove all instances", deployment.id);
        remove_all_instances(&mut deployment, &docker, "job", &intentional_shutdowns).await;
        return deployment;
    }

    // Terminal states: a one-shot job is done either way. Stop reconciling.
    // `Failed` is the job equivalent of `CrashLoopBackOff` for workers — set
    // below when restart_count hits MAX_RESTART_COUNT.
    if matches!(
        deployment.status,
        DeploymentStatus::Completed | DeploymentStatus::Failed
    ) {
        return deployment;
    }

    // Cap retries the same way workers do, but flip to `Failed` (terminal,
    // one-shot) instead of `CrashLoopBackOff` (long-running). A job that
    // never managed to boot after MAX_RESTART_COUNT tries is functionally
    // done — surface that to the operator.
    if deployment.restart_count >= MAX_RESTART_COUNT {
        deployment.status = DeploymentStatus::Failed;
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
    } else {
        // No instance: either we've never created one (Creating / Pending)
        // or the previous attempt left a transient error state (e.g.
        // create_container_error after Docker rejected `start`). Either
        // way, try again — the retry path is what eventually grows
        // restart_count past MAX_RESTART_COUNT and converges to Failed.
        match create_container(&mut deployment, &docker, &resolved_mounts).await {
            Ok(_) => {
                deployment.status = DeploymentStatus::Running;
            }
            Err(err) => {
                // `true` so the failure counts toward MAX_RESTART_COUNT.
                // Without this, the job loops on `create_container_error`
                // forever and the operator never sees a terminal state.
                handle_create_error(&mut deployment, err, true);
            }
        }
    }

    debug!("Job runtime apply {:?}", deployment.id);
    deployment
}

async fn handle_worker_deployment(
    mut deployment: Deployment,
    docker: Docker,
    resolved_mounts: Vec<ResolvedMount>,
    intentional_shutdowns: IntentionalShutdowns,
) -> Deployment {
    if deployment.status == DeploymentStatus::Deleted {
        debug!("{} marked as deleted. Remove all instances", deployment.id);
        remove_all_instances(&mut deployment, &docker, "worker", &intentional_shutdowns).await;
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
            Ordering::Less => {
                debug!(
                    "Scaling up: {} -> {} (creating 1 container)",
                    current_count, target_count
                );

                match create_container(&mut deployment, &docker, &resolved_mounts).await {
                    Ok(_) => {
                        deployment.emit_event(
                            "info",
                            format!(
                                "Scaled up from {} to {} replicas",
                                current_count,
                                current_count + 1
                            ),
                            "docker",
                            Some("scale_up"),
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
            Ordering::Greater => {
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
                    intentional_shutdowns.mark(container_id.clone()).await;
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
                        Some("scale_down"),
                    );
                }
            }
            Ordering::Equal => {
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
