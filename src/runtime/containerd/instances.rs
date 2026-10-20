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
use containerd_client::types::v1::{Process, Status as TaskStatus};
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

/// Status of each task, keyed by the container it runs in.
///
/// A task's `container_id` is not always filled in `ListTasks` (containerd 2.x
/// leaves it empty), and keying on it then matched no container: every running
/// instance was missed, and the scheduler created a new one on every cycle.
/// The init process of a task carries the container's id as its own `id`, so
/// that is the fallback.
fn task_status_by_container(tasks: Vec<Process>) -> HashMap<String, i32> {
    tasks
        .into_iter()
        .map(|t| {
            let container = if t.container_id.is_empty() {
                t.id
            } else {
                t.container_id
            };
            (container, t.status)
        })
        .collect()
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

/// The instances of a deployment split into live ones and ended ones: a
/// container whose task is stopped, was never started, or is missing.
pub(crate) async fn list_live_and_ended(
    client: &containerd_client::Client,
    namespace: &str,
    deployment_id: &str,
) -> Result<(Vec<String>, Vec<String>), RuntimeError> {
    let all = try_list_instances(client, namespace, deployment_id).await?;
    if all.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    let mut tasks = TasksClient::new(client.channel());
    let task_req = with_namespace!(
        ListTasksRequest {
            filter: String::new(),
        },
        namespace
    );
    let tasks = tasks
        .list(task_req)
        .await
        .map_err(|e| RuntimeError::Other(format!("ListTasks failed: {}", e)))?;
    let task_status = task_status_by_container(tasks.into_inner().tasks);
    Ok(split_live_and_ended(all, &task_status))
}

/// Every container of a deployment. A failed list call is an error rather
/// than an empty list, for callers that must not mistake one for the other.
pub(crate) async fn try_list_instances(
    client: &containerd_client::Client,
    namespace: &str,
    deployment_id: &str,
) -> Result<Vec<String>, RuntimeError> {
    Ok(
        list_instances_inner(client, namespace, deployment_id, "all")
            .await?
            .into_iter()
            .map(|(id, _)| id)
            .collect(),
    )
}

fn split_live_and_ended(
    containers: Vec<String>,
    task_status: &HashMap<String, i32>,
) -> (Vec<String>, Vec<String>) {
    containers
        .into_iter()
        .partition(|id| task_status.get(id).copied().is_some_and(is_active))
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
        Ok(resp) => task_status_by_container(resp.into_inner().tasks),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn task(container_id: &str, id: &str, status: TaskStatus) -> Process {
        Process {
            container_id: container_id.to_string(),
            id: id.to_string(),
            status: status as i32,
            ..Default::default()
        }
    }

    #[test]
    fn instances_without_an_active_task_have_ended() {
        let statuses = task_status_by_container(vec![
            task("c1", "c1", TaskStatus::Running),
            task("c2", "c2", TaskStatus::Stopped),
            task("c3", "c3", TaskStatus::Created),
        ]);
        let containers = ["c1", "c2", "c3", "c4"].map(String::from).to_vec();
        let (live, ended) = split_live_and_ended(containers, &statuses);
        assert_eq!(live, vec!["c1"]);
        assert_eq!(ended, vec!["c2", "c3", "c4"]);
    }

    #[test]
    fn tasks_are_keyed_by_their_container() {
        let statuses = task_status_by_container(vec![task("c1", "c1", TaskStatus::Running)]);
        assert_eq!(statuses.get("c1"), Some(&(TaskStatus::Running as i32)));
    }

    #[test]
    fn a_task_without_container_id_falls_back_to_its_id() {
        // What containerd 2.x returns from ListTasks.
        let statuses = task_status_by_container(vec![
            task("", "c1", TaskStatus::Running),
            task("", "c2", TaskStatus::Stopped),
        ]);
        assert!(statuses.get("c1").copied().is_some_and(is_active));
        assert!(!statuses.get("c2").copied().is_some_and(is_active));
        assert!(!statuses.contains_key(""));
    }
}
