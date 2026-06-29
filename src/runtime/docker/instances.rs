use bollard::Docker;
use bollard::models::ContainerSummaryStateEnum;
use bollard::query_parameters::ListContainersOptionsBuilder;

fn build_list_options(status: &str) -> bollard::query_parameters::ListContainersOptions {
    // Always list every container and filter by state client-side (see
    // `matches_status`). We deliberately do NOT push a server-side `status`
    // filter:
    // Podman's Docker-compatible API rejects `restarting` (it has no such
    // state) and fails the whole request, which `list_*` then swallows into an
    // empty Vec — making the scheduler believe a deployment has 0 live
    // instances and spawn one new container every tick, unbounded. Listing all
    // and filtering in-process behaves identically on Docker and Podman.
    let all = !matches!(status, "all");
    ListContainersOptionsBuilder::new().all(all).build()
}

/// Whether a container counts as a live instance for the given filter.
///
/// `active` = running or restarting. We deliberately drop `created`: a
/// container stuck in `created` (Docker accepted the spec but `start` failed —
/// e.g. the OCI runtime can't exec the binary) is *not* a live instance.
/// Counting it as active masked the failure — the scheduler saw
/// `current_count == target_count`, skipped the retry path, and `restart_count`
/// never climbed to `MAX_RESTART_COUNT`. With it excluded, the next tick sees
/// 0 instances, retries, increments `restart_count`, and eventually flips the
/// deployment to `CrashLoopBackOff` like any other crash loop.
fn matches_status(state: Option<&ContainerSummaryStateEnum>, status: &str) -> bool {
    match status {
        "all" => true,
        "active" => matches!(
            state,
            Some(ContainerSummaryStateEnum::RUNNING) | Some(ContainerSummaryStateEnum::RESTARTING)
        ),
        // An explicit single state filter (e.g. "exited", "created").
        s => state.map(|st| st.to_string() == s).unwrap_or(false),
    }
}

pub(crate) async fn list_instances(docker: &Docker, id: String, status: &str) -> Vec<String> {
    let options = build_list_options(status);
    let mut instances = Vec::new();

    match docker.list_containers(Some(options)).await {
        Ok(containers) => {
            for container in containers {
                if matches_status(container.state.as_ref(), status)
                    && let Some(labels) = container.labels
                    && let Some(deployment_id) = labels.get("ring_deployment")
                    && deployment_id == &id
                    && let Some(container_id) = container.id
                {
                    instances.push(container_id);
                }
            }
        }
        Err(e) => debug!("Docker list instances error: {}", e),
    }

    instances
}

/// Group running instance ids by deployment in a single host-wide list call,
/// instead of one `list_containers` per deployment. Only deployments in
/// `wanted` are kept. This is the bulk path behind the deployment listing: with
/// many deployments it collapses N full container lists into one.
pub(crate) async fn list_running_instances_grouped(
    docker: &Docker,
    wanted: &[String],
) -> std::collections::HashMap<String, Vec<String>> {
    use std::collections::{HashMap, HashSet};

    let wanted: HashSet<&str> = wanted.iter().map(String::as_str).collect();
    let mut grouped: HashMap<String, Vec<String>> = HashMap::new();

    let options = build_list_options("running");
    match docker.list_containers(Some(options)).await {
        Ok(containers) => {
            for container in containers {
                if matches_status(container.state.as_ref(), "running")
                    && let Some(labels) = &container.labels
                    && let Some(deployment_id) = labels.get("ring_deployment")
                    && wanted.contains(deployment_id.as_str())
                    && let Some(container_id) = &container.id
                {
                    grouped
                        .entry(deployment_id.clone())
                        .or_default()
                        .push(container_id.clone());
                }
            }
        }
        Err(e) => debug!("Docker list instances (grouped) error: {}", e),
    }

    grouped
}

pub(crate) async fn list_instances_with_names(
    docker: &Docker,
    id: String,
    status: &str,
) -> Vec<(String, String)> {
    let options = build_list_options(status);
    let mut instances = Vec::new();

    match docker.list_containers(Some(options)).await {
        Ok(containers) => {
            for container in containers {
                if matches_status(container.state.as_ref(), status)
                    && let Some(labels) = &container.labels
                    && let Some(deployment_id) = labels.get("ring_deployment")
                    && deployment_id == &id
                    && let Some(container_id) = &container.id
                {
                    let name = container
                        .names
                        .as_ref()
                        .and_then(|names| names.first())
                        .map(|n| n.trim_start_matches('/').to_string())
                        .unwrap_or_else(|| container_id.chars().take(12).collect());
                    instances.push((container_id.clone(), name));
                }
            }
        }
        Err(e) => debug!("Docker list instances error: {}", e),
    }

    instances
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_counts_running_and_restarting() {
        assert!(matches_status(
            Some(&ContainerSummaryStateEnum::RUNNING),
            "active"
        ));
        assert!(matches_status(
            Some(&ContainerSummaryStateEnum::RESTARTING),
            "active"
        ));
    }

    #[test]
    fn active_excludes_created_and_exited() {
        // A `created` container (start failed) is NOT a live instance — this is
        // the exclusion that lets the scheduler retry and eventually reach
        // CrashLoopBackOff instead of treating it as healthy.
        assert!(!matches_status(
            Some(&ContainerSummaryStateEnum::CREATED),
            "active"
        ));
        assert!(!matches_status(
            Some(&ContainerSummaryStateEnum::EXITED),
            "active"
        ));
        assert!(!matches_status(None, "active"));
    }

    #[test]
    fn all_matches_any_state() {
        assert!(matches_status(
            Some(&ContainerSummaryStateEnum::CREATED),
            "all"
        ));
        assert!(matches_status(None, "all"));
    }

    #[test]
    fn explicit_state_filter_matches_by_name() {
        assert!(matches_status(
            Some(&ContainerSummaryStateEnum::EXITED),
            "exited"
        ));
        assert!(!matches_status(
            Some(&ContainerSummaryStateEnum::RUNNING),
            "exited"
        ));
        assert!(!matches_status(None, "exited"));
    }
}
