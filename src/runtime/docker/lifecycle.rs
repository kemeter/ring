use super::container::{create_container, remove_container};
use super::instances::list_instances;
use crate::hypervisor::error::RuntimeError;
use crate::hypervisor::types::InstanceStatus;
use crate::models::deployments::{Deployment, DeploymentStatus, MAX_RESTART_COUNT};
use crate::models::volume::ResolvedMount;
use crate::runtime::registry_auth::HostAuthSettings;
use crate::scheduler::intentional_shutdowns::IntentionalShutdowns;
use bollard::Docker;
use bollard::query_parameters::InspectContainerOptions;

/// Reconcile a Docker/Podman deployment through the legacy `apply` path: jobs,
/// and the teardown of deleted deployments. Workers are reconciled by the
/// scheduler through [`super::docker_lifecycle::DockerLifecycle`]'s instance
/// driver, which owns their restart policy.
pub(crate) async fn apply(
    mut deployment: Deployment,
    docker: Docker,
    resolved_mounts: Vec<ResolvedMount>,
    intentional_shutdowns: IntentionalShutdowns,
    host_auth: HostAuthSettings,
) -> Deployment {
    let status_filter = if deployment.status == DeploymentStatus::Deleted {
        "all"
    } else {
        "active"
    };
    deployment.instances = list_instances(&docker, deployment.id.to_string(), status_filter).await;

    if deployment.status == DeploymentStatus::Deleted {
        debug!("{} marked as deleted. Remove all instances", deployment.id);
        let kind = deployment.kind.clone();
        remove_all_instances(&mut deployment, &docker, &kind, &intentional_shutdowns).await;
        return deployment;
    }

    if deployment.kind == "job" {
        return handle_job_deployment(deployment, docker, resolved_mounts, host_auth).await;
    }

    warn!(
        "Worker {} reached the Docker apply path; workers are reconciled by the scheduler",
        deployment.id
    );
    deployment
}

fn handle_create_error(deployment: &mut Deployment, err: RuntimeError, increment_restart: bool) {
    // Decide retry-vs-give-up before mapping the message. A terminal error (the
    // image truly doesn't exist, config is missing, the container spec is
    // rejected) can't fix itself on a retry, so instead of bumping
    // restart_count by one and burning five reconcile cycles, jump straight to
    // the restart bound: the deployment converges to its terminal state on the
    // next tick instead of five ticks from now. Transient errors still bump by
    // one and retry within the budget, exactly as before.
    //
    // Exceptions: a terminal status the scheduler no longer reconciles needs
    // no budget marker, and a host-memory refusal counts as one attempt only,
    // since memory is freed all the time and the refusal is retried.
    let disposition = crate::hypervisor::classifier::classify_create_error(&err);
    let terminal = disposition.is_terminal();
    let marker_needed = match &disposition {
        crate::hypervisor::classifier::Disposition::Terminal(status) => {
            !crate::hypervisor::classifier::scheduler_skips_by_status(status)
        }
        crate::hypervisor::classifier::Disposition::Retry => true,
    };

    let refused_for_memory = matches!(err, RuntimeError::InsufficientResources(_));
    if increment_restart && marker_needed {
        if terminal && !refused_for_memory {
            deployment.restart_count = MAX_RESTART_COUNT;
        } else {
            deployment.restart_count += 1;
        }
    }

    let (status, reason, message) =
        crate::hypervisor::classifier::create_error_outcome(&err, deployment);

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
    host_auth: HostAuthSettings,
) -> Deployment {
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
        match create_container(&mut deployment, &docker, &resolved_mounts, &host_auth).await {
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
