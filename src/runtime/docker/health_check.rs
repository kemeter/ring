//! Docker-specific bits of the health-check pipeline.
//!
//! TCP and HTTP probes themselves live in `runtime::health_probes` and are
//! shared with the Cloud Hypervisor runtime (and any future VM runtime). The
//! Docker runtime only contributes:
//!
//! - `container_address` — resolve a container's reachable IP from
//!   `bollard::inspect_container`. Used by the trait-default
//!   `execute_health_check` once it knows it has a TCP/HTTP probe to run.
//! - `execute_command_check` — run a shell command inside the container via
//!   `docker exec`. The VM runtimes have no equivalent, so this stays in
//!   the Docker module.

use crate::models::health_check::HealthCheckStatus;
use bollard::Docker;
use bollard::exec::{CreateExecOptions, StartExecOptions};
use bollard::query_parameters::InspectContainerOptions;
use std::net::IpAddr;

/// Resolve a Docker container's IP from its network settings.
///
/// Prefers the default `bridge` network (matches Docker's own conventions
/// when no user-defined network is attached). Falls back to the first
/// network with a non-empty IP if `bridge` is missing — Ring containers
/// live on `ring_<namespace>` networks, so this is the common path.
pub(crate) async fn container_address(docker: &Docker, container_id: &str) -> Option<IpAddr> {
    let inspect_result = docker
        .inspect_container(container_id, None::<InspectContainerOptions>)
        .await
        .ok()?;

    let networks = inspect_result.network_settings?.networks?;

    if let Some(bridge) = networks.get("bridge")
        && let Some(ip) = &bridge.ip_address
        && !ip.is_empty()
        && let Ok(parsed) = ip.parse::<IpAddr>()
    {
        return Some(parsed);
    }

    for (_, network) in networks {
        if let Some(ip) = network.ip_address
            && !ip.is_empty()
            && let Ok(parsed) = ip.parse::<IpAddr>()
        {
            return Some(parsed);
        }
    }

    None
}

/// Run a `command` health-check probe via `docker exec`. Success means the
/// command was successfully started — Docker's exec API doesn't surface the
/// exit code synchronously, so a successful start is treated as a healthy
/// probe (matches the historical behavior of this runtime).
pub(crate) async fn execute_command_check(
    docker: &Docker,
    container_id: &str,
    command: &str,
) -> (HealthCheckStatus, Option<String>) {
    let cmd_parts = match shell_words::split(command) {
        Ok(parts) if parts.is_empty() => {
            return (HealthCheckStatus::Failed, Some("Empty command".to_string()));
        }
        Ok(parts) => parts,
        Err(e) => {
            return (
                HealthCheckStatus::Failed,
                Some(format!("Invalid command syntax: {}", e)),
            );
        }
    };

    let exec_options = CreateExecOptions {
        cmd: Some(cmd_parts),
        attach_stdout: Some(true),
        attach_stderr: Some(true),
        ..Default::default()
    };

    let exec_result = match docker.create_exec(container_id, exec_options).await {
        Ok(result) => result,
        Err(e) => {
            return (
                HealthCheckStatus::Failed,
                Some(format!("Failed to create exec: {}", e)),
            );
        }
    };

    let start_exec_options = StartExecOptions {
        detach: false,
        ..Default::default()
    };

    match docker
        .start_exec(&exec_result.id, Some(start_exec_options))
        .await
    {
        Ok(_) => (
            HealthCheckStatus::Success,
            Some("Command executed successfully".to_string()),
        ),
        Err(e) => (
            HealthCheckStatus::Failed,
            Some(format!("Failed to execute command: {}", e)),
        ),
    }
}
