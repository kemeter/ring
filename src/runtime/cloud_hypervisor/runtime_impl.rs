use crate::api::dto::stats::ContainerStatsOutput;
use crate::models::deployments::Deployment;
use crate::models::health_check::{HealthCheck, HealthCheckStatus};
use crate::runtime::cloud_hypervisor::client::CloudHypervisorClient;
use crate::runtime::runtime::{Log, RuntimeInterface};
use async_trait::async_trait;
use axum::response::sse::Event;
use futures::stream::{self, Stream};
use std::collections::HashMap;
use std::convert::Infallible;
use std::path::PathBuf;
use std::pin::Pin;
use tokio::sync::RwLock;

pub struct CloudHypervisorRuntime {
    deployment: Deployment,
    socket_dir: String,
    instances: std::sync::Arc<RwLock<HashMap<String, Vec<String>>>>,
}

impl CloudHypervisorRuntime {
    pub fn new(
        deployment: Deployment,
        socket_dir: String,
        instances: std::sync::Arc<RwLock<HashMap<String, Vec<String>>>>,
    ) -> Self {
        Self {
            deployment,
            socket_dir,
            instances,
        }
    }

    fn socket_path(&self, instance_id: &str) -> PathBuf {
        PathBuf::from(&self.socket_dir).join(format!("{}.sock", instance_id))
    }
}

#[async_trait]
impl RuntimeInterface for CloudHypervisorRuntime {
    async fn list_instances(&self) -> Vec<String> {
        let map = self.instances.read().await;
        map.get(&self.deployment.id).cloned().unwrap_or_default()
    }

    async fn list_instances_with_names(&self) -> Vec<(String, String)> {
        let instances = self.list_instances().await;
        instances
            .into_iter()
            .map(|id| {
                let name = format!(
                    "{}_{}",
                    self.deployment.name,
                    &id[..12.min(id.len())]
                );
                (id, name)
            })
            .collect()
    }

    async fn get_logs(
        &self,
        tail: Option<&str>,
        _since: Option<i32>,
        _container: Option<&str>,
    ) -> Vec<Log> {
        // Read logs from the VM's serial console output file
        let mut logs = Vec::new();
        let instances = self.list_instances().await;

        for instance_id in instances {
            let log_path = PathBuf::from(&self.socket_dir)
                .join(format!("{}.log", instance_id));

            if let Ok(content) = tokio::fs::read_to_string(&log_path).await {
                let lines: Vec<&str> = content.lines().collect();
                let tail_count: usize = tail
                    .and_then(|t| t.parse().ok())
                    .unwrap_or(lines.len());
                let start = lines.len().saturating_sub(tail_count);

                for line in &lines[start..] {
                    logs.push(Log {
                        instance: instance_id.clone(),
                        level: "info".to_string(),
                        timestamp: None,
                        message: line.to_string(),
                    });
                }
            }
        }

        logs
    }

    async fn stream_logs(
        &self,
        _tail: Option<&str>,
        _since: Option<i32>,
        _container: Option<&str>,
    ) -> Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>> {
        // TODO: implement real log streaming via vsock or serial follow
        Box::pin(stream::empty())
    }

    async fn execute_health_check(
        &self,
        instance_id: &str,
        health_check: &HealthCheck,
    ) -> (HealthCheckStatus, Option<String>) {
        let socket = self.socket_path(instance_id);
        let socket_str = socket.to_str().unwrap_or_default();

        // First check if the VM is running
        let client = CloudHypervisorClient::new(socket_str);
        match client.info().await {
            Ok(info) => {
                if info.state != "Running" {
                    return (
                        HealthCheckStatus::Failed,
                        Some(format!("VM is not running (state: {})", info.state)),
                    );
                }
            }
            Err(e) => {
                return (
                    HealthCheckStatus::Failed,
                    Some(format!("Failed to get VM info: {}", e)),
                );
            }
        }

        // For TCP and HTTP checks, delegate to the same logic as Docker
        // since they work over the network regardless of runtime
        match health_check {
            HealthCheck::Tcp { port, timeout, .. } => {
                let timeout_duration = HealthCheck::parse_duration(timeout)
                    .unwrap_or(std::time::Duration::from_secs(5));

                // TODO: resolve VM IP from TAP device
                // For now, try connecting on localhost
                match tokio::time::timeout(
                    timeout_duration,
                    tokio::net::TcpStream::connect(format!("127.0.0.1:{}", port)),
                )
                .await
                {
                    Ok(Ok(_)) => (HealthCheckStatus::Success, None),
                    Ok(Err(e)) => (
                        HealthCheckStatus::Failed,
                        Some(format!("TCP connection failed: {}", e)),
                    ),
                    Err(_) => (
                        HealthCheckStatus::Timeout,
                        Some("TCP health check timed out".to_string()),
                    ),
                }
            }
            HealthCheck::Http { url, timeout, .. } => {
                let timeout_duration = HealthCheck::parse_duration(timeout)
                    .unwrap_or(std::time::Duration::from_secs(5));

                let client = reqwest::Client::builder()
                    .timeout(timeout_duration)
                    .build()
                    .unwrap_or_default();

                match client.get(url).send().await {
                    Ok(resp) if resp.status().is_success() => (HealthCheckStatus::Success, None),
                    Ok(resp) => (
                        HealthCheckStatus::Failed,
                        Some(format!("HTTP status: {}", resp.status())),
                    ),
                    Err(e) => (
                        HealthCheckStatus::Failed,
                        Some(format!("HTTP request failed: {}", e)),
                    ),
                }
            }
            HealthCheck::Command { .. } => {
                // Command health checks would need to exec inside the VM
                // Not supported yet for Cloud Hypervisor
                (
                    HealthCheckStatus::Failed,
                    Some("Command health checks not supported for Cloud Hypervisor VMs".to_string()),
                )
            }
        }
    }

    async fn remove_instance(&self, instance_id: &str) {
        let socket = self.socket_path(instance_id);
        let socket_str = socket.to_str().unwrap_or_default();

        if socket.exists() {
            let client = CloudHypervisorClient::new(socket_str);
            let _ = client.shutdown_vm().await;
            let _ = client.delete_vm().await;
            let _ = tokio::fs::remove_file(&socket).await;
        }

        super::network::teardown_network(instance_id).await;
    }

    async fn get_instance_stats(&self) -> Vec<ContainerStatsOutput> {
        // Cloud Hypervisor doesn't expose per-VM stats via API in the same way
        // TODO: read from cgroups or /proc for the cloud-hypervisor process
        vec![]
    }
}
