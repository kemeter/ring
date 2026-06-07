//! Listing Ring-managed containerd instances.
//!
//! Containers are tagged with the [`RING_DEPLOYMENT_LABEL`] at create time, so
//! we list by containerd's label filter and cross-check the task status. The
//! "active" vs "all" semantics mirror the Docker runtime: an "active" instance
//! is one whose task is actually `Running`, so a container whose task never
//! started (or already exited) is not counted toward the replica target and the
//! scheduler retries.

use super::RING_DEPLOYMENT_LABEL;
use crate::hypervisor::error::RuntimeError;
use containerd_client::services::v1::ListContainersRequest;
use containerd_client::services::v1::ListTasksRequest;
use containerd_client::services::v1::containers_client::ContainersClient;
use containerd_client::services::v1::tasks_client::TasksClient;
use containerd_client::types::v1::Status as TaskStatus;
use containerd_client::with_namespace;
use std::collections::HashMap;
use tonic::Request;

/// Map a containerd task `Status` to whether the instance counts as "active".
fn is_active(status: i32) -> bool {
    matches!(
        TaskStatus::try_from(status),
        Ok(TaskStatus::Running) | Ok(TaskStatus::Paused) | Ok(TaskStatus::Pausing)
    )
}

/// List container ids for a deployment, filtered by `status` ("all" or
/// "active"). Returns the containerd container ids (which are also the Ring
/// instance ids).
pub(crate) async fn list_instances(
    client: &containerd_client::Client,
    namespace: &str,
    deployment_id: &str,
    status: &str,
) -> Vec<String> {
    match list_instances_inner(client, namespace, deployment_id, status).await {
        Ok(ids) => ids.into_iter().map(|(id, _)| id).collect(),
        Err(e) => {
            debug!("containerd list instances error: {}", e);
            Vec::new()
        }
    }
}

/// Like [`list_instances`] but returns `(id, name)` pairs. The container id is
/// already the human-readable `<namespace>_<name>_<suffix>` we set at creation,
/// so name == id here.
pub(crate) async fn list_instances_with_names(
    client: &containerd_client::Client,
    namespace: &str,
    deployment_id: &str,
    status: &str,
) -> Vec<(String, String)> {
    match list_instances_inner(client, namespace, deployment_id, status).await {
        Ok(ids) => ids,
        Err(e) => {
            debug!("containerd list instances error: {}", e);
            Vec::new()
        }
    }
}

async fn list_instances_inner(
    client: &containerd_client::Client,
    namespace: &str,
    deployment_id: &str,
    status: &str,
) -> Result<Vec<(String, String)>, RuntimeError> {
    let mut containers = ContainersClient::new(client.channel());
    // containerd filter syntax: match the ring_deployment label exactly.
    let filter = format!("labels.\"{}\"=={}", RING_DEPLOYMENT_LABEL, deployment_id);
    let req = with_namespace!(
        ListContainersRequest {
            filters: vec![filter],
        },
        namespace
    );
    let resp = containers
        .list(req)
        .await
        .map_err(|e| RuntimeError::Other(format!("ListContainers failed: {}", e)))?;
    let container_ids: Vec<String> = resp
        .into_inner()
        .containers
        .into_iter()
        .map(|c| c.id)
        .collect();

    if status == "all" {
        return Ok(container_ids
            .into_iter()
            .map(|id| (id.clone(), id))
            .collect());
    }

    // "active": cross-check against running tasks.
    let mut tasks = TasksClient::new(client.channel());
    let task_req = with_namespace!(
        ListTasksRequest {
            filter: String::new(),
        },
        namespace
    );
    let task_status: HashMap<String, i32> = match tasks.list(task_req).await {
        Ok(resp) => resp
            .into_inner()
            .tasks
            .into_iter()
            .map(|t| (t.container_id, t.status))
            .collect(),
        Err(e) => {
            debug!("containerd ListTasks failed: {}", e);
            HashMap::new()
        }
    };

    Ok(container_ids
        .into_iter()
        .filter(|id| task_status.get(id).copied().is_some_and(is_active))
        .map(|id| (id.clone(), id))
        .collect())
}
