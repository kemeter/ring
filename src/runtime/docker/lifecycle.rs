use super::container::remove_container;
use super::instances::list_instances;
use crate::models::deployments::{Deployment, DeploymentStatus};
use crate::scheduler::intentional_shutdowns::IntentionalShutdowns;
use bollard::Docker;

/// Tear down a deleted Docker/Podman deployment. Everything else is reconciled
/// by the scheduler through [`super::docker_lifecycle::DockerLifecycle`]'s
/// instance driver, which leaves the restart policy to the scheduler.
pub(crate) async fn apply(
    mut deployment: Deployment,
    docker: Docker,
    intentional_shutdowns: IntentionalShutdowns,
) -> Deployment {
    if deployment.status != DeploymentStatus::Deleted {
        warn!(
            "Deployment {} reached the Docker apply path; it is reconciled by the scheduler",
            deployment.id
        );
        return deployment;
    }

    debug!("{} marked as deleted. Remove all instances", deployment.id);
    deployment.instances = list_instances(&docker, deployment.id.to_string(), "all").await;
    let kind = deployment.kind.clone();
    remove_all_instances(&mut deployment, &docker, &kind, &intentional_shutdowns).await;
    deployment
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
