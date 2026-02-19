use bollard::Docker;
use bollard::query_parameters::ListContainersOptionsBuilder;
use std::collections::HashMap;

fn build_list_options(status: &str) -> bollard::query_parameters::ListContainersOptions {
    match status {
        "all" => ListContainersOptionsBuilder::new().all(true).build(),
        "active" => {
            let filters = HashMap::from([
                ("status".to_string(), vec![
                    "running".to_string(),
                    "created".to_string(),
                    "restarting".to_string(),
                ])
            ]);
            ListContainersOptionsBuilder::new().all(true).filters(&filters).build()
        }
        s => {
            let filters = HashMap::from([("status".to_string(), vec![s.to_string()])]);
            ListContainersOptionsBuilder::new().all(false).filters(&filters).build()
        }
    }
}

pub(crate) async fn list_instances(docker: &Docker, id: String, status: &str) -> Vec<String> {
    let options = build_list_options(status);
    let mut instances = Vec::new();

    match docker.list_containers(Some(options)).await {
        Ok(containers) => {
            for container in containers {
                if let Some(labels) = container.labels {
                    if let Some(deployment_id) = labels.get("ring_deployment") {
                        if deployment_id == &id {
                            if let Some(container_id) = container.id {
                                instances.push(container_id);
                            }
                        }
                    }
                }
            }
        }
        Err(e) => debug!("Docker list instances error: {}", e),
    }

    instances
}

pub(crate) async fn list_instances_with_names(docker: &Docker, id: String, status: &str) -> Vec<(String, String)> {
    let options = build_list_options(status);
    let mut instances = Vec::new();

    match docker.list_containers(Some(options)).await {
        Ok(containers) => {
            for container in containers {
                if let Some(labels) = &container.labels {
                    if let Some(deployment_id) = labels.get("ring_deployment") {
                        if deployment_id == &id {
                            if let Some(container_id) = &container.id {
                                let name = container.names
                                    .as_ref()
                                    .and_then(|names| names.first())
                                    .map(|n| n.trim_start_matches('/').to_string())
                                    .unwrap_or_else(|| container_id.chars().take(12).collect());
                                instances.push((container_id.clone(), name));
                            }
                        }
                    }
                }
            }
        }
        Err(e) => debug!("Docker list instances error: {}", e),
    }

    instances
}
