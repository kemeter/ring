//! Health-check primitives for the containerd runtime.
//!
//! - [`instance_address`] resolves the CNI-assigned IP so the trait-default
//!   `execute_health_check` can run TCP/HTTP probes.
//! - [`execute_command_check`] runs a `command` probe inside the task via
//!   `Tasks.Exec`, equivalent to `docker exec`.
//!
//! `instance_address` reads the IP that `host-local` IPAM recorded for the
//! container at CNI ADD time. host-local writes one file per allocated address
//! under its data dir, named by the IP and containing the container id, so we
//! reverse-map the container id to its IP there. This avoids having to keep the
//! CNI ADD result in memory across the daemon's lifetime.

use crate::models::health_check::HealthCheckStatus;
use containerd_client::services::v1::tasks_client::TasksClient;
use containerd_client::services::v1::{
    DeleteProcessRequest, ExecProcessRequest, GetRequest, WaitRequest,
};
use containerd_client::types::v1::Status as TaskStatus;
use containerd_client::with_namespace;
use std::net::IpAddr;
use std::path::Path;
use tonic::Request;

/// host-local IPAM data directory for Ring's CNI network. The plugin stores one
/// file per allocated IP here (named by the address); each file's contents are
/// the owning container id.
const HOST_LOCAL_DIR: &str = "/var/lib/cni/networks/ring";

/// Resolve the container's CNI IP by scanning the host-local IPAM store for the
/// file whose contents reference this container id.
pub(crate) async fn instance_address(
    _client: &containerd_client::Client,
    _namespace: &str,
    instance_id: &str,
) -> Option<IpAddr> {
    let dir = Path::new(HOST_LOCAL_DIR);
    let mut entries = tokio::fs::read_dir(dir).await.ok()?;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        // Only consider files that parse as an IP address.
        let Ok(ip) = name.parse::<IpAddr>() else {
            continue;
        };
        if let Ok(contents) = tokio::fs::read_to_string(entry.path()).await {
            // host-local writes the container id (sometimes followed by the
            // interface name on a second line) into the file.
            if contents.lines().any(|l| l.trim() == instance_id) {
                return Some(ip);
            }
        }
    }
    None
}

/// Run a `command` probe inside the task via `Tasks.Exec`, reporting `Success`
/// on exit code 0 and `Failed` otherwise. The exec process gets a unique id; we
/// `Wait` on it for the exit status, then `DeleteProcess` to reap it.
pub(crate) async fn execute_command_check(
    client: &containerd_client::Client,
    namespace: &str,
    instance_id: &str,
    command: &str,
) -> (HealthCheckStatus, Option<String>) {
    let args = match shell_words::split(command) {
        Ok(a) if a.is_empty() => {
            return (HealthCheckStatus::Failed, Some("Empty command".to_string()));
        }
        Ok(a) => a,
        Err(e) => {
            return (
                HealthCheckStatus::Failed,
                Some(format!("Invalid command syntax: {}", e)),
            );
        }
    };

    // Refuse to exec into a task that is not running — Exec would error opaquely.
    if !task_running(client, namespace, instance_id).await {
        return (
            HealthCheckStatus::Failed,
            Some("task is not running".to_string()),
        );
    }

    let exec_id = format!("ring-hc-{}", super::tiny_id());
    let spec = super::oci::build_exec_process_spec(args);

    let mut tasks = TasksClient::new(client.channel());
    let exec_req = with_namespace!(
        ExecProcessRequest {
            container_id: instance_id.to_string(),
            stdin: String::new(),
            stdout: String::new(),
            stderr: String::new(),
            terminal: false,
            spec: Some(spec),
            exec_id: exec_id.clone(),
        },
        namespace
    );
    if let Err(e) = tasks.exec(exec_req).await {
        return (
            HealthCheckStatus::Failed,
            Some(format!("Failed to create exec: {}", e)),
        );
    }

    // Start the exec process.
    let start_req = with_namespace!(
        containerd_client::services::v1::StartRequest {
            container_id: instance_id.to_string(),
            exec_id: exec_id.clone(),
        },
        namespace
    );
    if let Err(e) = tasks.start(start_req).await {
        cleanup_exec(&mut tasks, namespace, instance_id, &exec_id).await;
        return (
            HealthCheckStatus::Failed,
            Some(format!("Failed to start exec: {}", e)),
        );
    }

    // Wait for completion and read the exit status. Bound the wait: a probe
    // command that hangs (e.g. `sleep 9999`) would otherwise block this call
    // forever, since Task.Wait has no built-in deadline. The trait dispatch
    // doesn't thread the per-probe timeout down here, so we apply a defensive
    // upper bound — a hung probe must fail, not wedge the health loop.
    let wait_req = with_namespace!(
        WaitRequest {
            container_id: instance_id.to_string(),
            exec_id: exec_id.clone(),
        },
        namespace
    );
    let outcome = match tokio::time::timeout(EXEC_WAIT_TIMEOUT, tasks.wait(wait_req)).await {
        Ok(Ok(resp)) => match resp.into_inner().exit_status {
            0 => (
                HealthCheckStatus::Success,
                Some("Command exited 0".to_string()),
            ),
            code => (
                HealthCheckStatus::Failed,
                Some(format!("Command exited {}", code)),
            ),
        },
        Ok(Err(e)) => (
            HealthCheckStatus::Failed,
            Some(format!("Failed to wait for exec: {}", e)),
        ),
        Err(_) => {
            // Timed out: SIGKILL the hung exec process so it doesn't linger.
            let _ = tasks
                .kill(with_namespace!(
                    containerd_client::services::v1::KillRequest {
                        container_id: instance_id.to_string(),
                        exec_id: exec_id.clone(),
                        signal: 9,
                        all: false,
                    },
                    namespace
                ))
                .await;
            (
                HealthCheckStatus::Failed,
                Some(format!(
                    "Command probe timed out after {}s",
                    EXEC_WAIT_TIMEOUT.as_secs()
                )),
            )
        }
    };

    cleanup_exec(&mut tasks, namespace, instance_id, &exec_id).await;
    outcome
}

/// Defensive upper bound for a `command` probe's exec. The configured per-probe
/// timeout isn't threaded into the trait's `execute_command_probe`, so this
/// guards against a probe that never returns.
const EXEC_WAIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

async fn cleanup_exec(
    tasks: &mut TasksClient<tonic::transport::Channel>,
    namespace: &str,
    instance_id: &str,
    exec_id: &str,
) {
    let req = with_namespace!(
        DeleteProcessRequest {
            container_id: instance_id.to_string(),
            exec_id: exec_id.to_string(),
        },
        namespace
    );
    let _ = tasks.delete_process(req).await;
}

async fn task_running(
    client: &containerd_client::Client,
    namespace: &str,
    instance_id: &str,
) -> bool {
    let mut tasks = TasksClient::new(client.channel());
    let req = with_namespace!(
        GetRequest {
            container_id: instance_id.to_string(),
            exec_id: String::new(),
        },
        namespace
    );
    match tasks.get(req).await {
        Ok(resp) => resp
            .into_inner()
            .process
            .map(|p| matches!(TaskStatus::try_from(p.status), Ok(TaskStatus::Running)))
            .unwrap_or(false),
        Err(_) => false,
    }
}
