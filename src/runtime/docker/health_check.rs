use bollard::Docker;
use bollard::query_parameters::InspectContainerOptions;
use bollard::exec::{CreateExecOptions, StartExecOptions};
use crate::models::health_check::{HealthCheck, HealthCheckStatus};

pub(crate) async fn execute_health_check_for_instance(docker: &Docker, container_id: String, health_check: HealthCheck) -> (HealthCheckStatus, Option<String>) {
    let container_ip = match health_check {
        HealthCheck::Command { .. } => None,
        _ => get_container_ip(docker, &container_id).await,
    };

    match health_check {
        HealthCheck::Tcp { port, .. } => {
            match container_ip {
                Some(ip) => execute_tcp_check(&ip, port).await,
                None => (HealthCheckStatus::Failed, Some(format!("Could not get IP for container {}", container_id))),
            }
        }
        HealthCheck::Http { url, .. } => {
            match container_ip {
                Some(ip) => execute_http_check(&ip, &url).await,
                None => (HealthCheckStatus::Failed, Some(format!("Could not get IP for container {}", container_id))),
            }
        }
        HealthCheck::Command { command, .. } => {
            execute_command_check(docker, &container_id, &command).await
        }
    }
}

async fn get_container_ip(docker: &Docker, container_id: &str) -> Option<String> {
    let inspect_result = docker.inspect_container(container_id, None::<InspectContainerOptions>).await.ok()?;

    if let Some(networks) = inspect_result.network_settings?.networks {
        if let Some(bridge) = networks.get("bridge") {
            if let Some(ip) = &bridge.ip_address {
                if !ip.is_empty() {
                    return Some(ip.clone());
                }
            }
        }

        for (_, network) in networks {
            if let Some(ip) = network.ip_address {
                if !ip.is_empty() {
                    return Some(ip);
                }
            }
        }
    }

    None
}

async fn execute_tcp_check(container_ip: &str, port: u16) -> (HealthCheckStatus, Option<String>) {
    use tokio::net::TcpStream;
    use std::time::Duration;

    let addr = format!("{}:{}", container_ip, port);

    match tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(&addr)).await {
        Ok(Ok(_)) => (HealthCheckStatus::Success, Some(format!("TCP connection to {} successful", addr))),
        Ok(Err(e)) => (HealthCheckStatus::Failed, Some(format!("TCP connection failed: {}", e))),
        Err(_) => (HealthCheckStatus::Failed, Some(format!("TCP connection timed out for {}", addr))),
    }
}

async fn execute_http_check(container_ip: &str, url: &str) -> (HealthCheckStatus, Option<String>) {
    let target_url = url.replace("localhost", container_ip);

    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(e) => return (HealthCheckStatus::Failed, Some(format!("Failed to create HTTP client: {}", e))),
    };

    match client.get(&target_url).send().await {
        Ok(response) => {
            let code = response.status().as_u16();
            if (200..300).contains(&code) {
                (HealthCheckStatus::Success, Some(format!("HTTP check successful ({}) for {}", code, target_url)))
            } else {
                (HealthCheckStatus::Failed, Some(format!("HTTP check failed with status {} for {}", code, target_url)))
            }
        }
        Err(e) => (HealthCheckStatus::Failed, Some(format!("HTTP request failed for {}: {}", target_url, e))),
    }
}

async fn execute_command_check(docker: &Docker, container_id: &str, command: &str) -> (HealthCheckStatus, Option<String>) {
    let cmd_parts = match shell_words::split(command) {
        Ok(parts) if parts.is_empty() => {
            return (HealthCheckStatus::Failed, Some("Empty command".to_string()));
        }
        Ok(parts) => parts,
        Err(e) => {
            return (HealthCheckStatus::Failed, Some(format!("Invalid command syntax: {}", e)));
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
            return (HealthCheckStatus::Failed, Some(format!("Failed to create exec: {}", e)));
        }
    };

    let start_exec_options = StartExecOptions {
        detach: false,
        ..Default::default()
    };

    match docker.start_exec(&exec_result.id, Some(start_exec_options)).await {
        Ok(_) => (HealthCheckStatus::Success, Some("Command executed successfully".to_string())),
        Err(e) => (HealthCheckStatus::Failed, Some(format!("Failed to execute command: {}", e))),
    }
}
