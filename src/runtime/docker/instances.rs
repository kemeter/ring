use bollard::Docker;
use bollard::query_parameters::ListContainersOptionsBuilder;
use std::collections::HashMap;

fn build_list_options(status: &str) -> bollard::query_parameters::ListContainersOptions {
    match status {
        "all" => ListContainersOptionsBuilder::new().all(true).build(),
        "active" => {
            // We deliberately drop `created` here. A container stuck in
            // `created` (Docker accepted the spec but `start` failed — e.g.
            // OCI runtime can't exec the binary) is *not* a live instance.
            // Counting it as active masked the failure: the scheduler saw
            // `current_count == target_count`, skipped the retry path, and
            // restart_count never climbed to MAX_RESTART_COUNT. With it out
            // of the filter, the next tick sees 0 instances, re-tries,
            // increments restart_count, and eventually flips the deployment
            // to CrashLoopBackOff like any other crash loop.
            let filters = HashMap::from([(
                "status".to_string(),
                vec!["running".to_string(), "restarting".to_string()],
            )]);
            ListContainersOptionsBuilder::new()
                .all(true)
                .filters(&filters)
                .build()
        }
        s => {
            let filters = HashMap::from([("status".to_string(), vec![s.to_string()])]);
            ListContainersOptionsBuilder::new()
                .all(false)
                .filters(&filters)
                .build()
        }
    }
}

pub(crate) async fn list_instances(docker: &Docker, id: String, status: &str) -> Vec<String> {
    let options = build_list_options(status);
    let mut instances = Vec::new();

    match docker.list_containers(Some(options)).await {
        Ok(containers) => {
            for container in containers {
                if let Some(labels) = container.labels
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
                if let Some(labels) = &container.labels
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
