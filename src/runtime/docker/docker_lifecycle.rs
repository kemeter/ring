use crate::api::dto::stats::InstanceStatsOutput;
use crate::models::deployments::Deployment;
use crate::models::health_check::HealthCheckStatus;
use crate::models::volume::ResolvedMount;
use crate::hypervisor::lifecycle_trait::{Log, RuntimeLifecycle, classify_log, extract_date};
use crate::scheduler::intentional_shutdowns::IntentionalShutdowns;
use async_trait::async_trait;
use axum::response::sse::Event;
use bollard::Docker;
use futures::stream::{self, Stream, StreamExt};
use std::convert::Infallible;
use std::pin::Pin;

fn filter_instances(
    instances: Vec<(String, String)>,
    filter: Option<&str>,
) -> Vec<(String, String)> {
    match filter {
        Some(f) => instances
            .into_iter()
            .filter(|(id, name)| id.starts_with(f) || name.contains(f))
            .collect(),
        None => instances,
    }
}

pub struct DockerLifecycle {
    docker: Docker,
    intentional_shutdowns: IntentionalShutdowns,
}

impl DockerLifecycle {
    pub fn new(docker: Docker, intentional_shutdowns: IntentionalShutdowns) -> Self {
        Self {
            docker,
            intentional_shutdowns,
        }
    }
}

#[async_trait]
impl RuntimeLifecycle for DockerLifecycle {
    async fn apply(
        &self,
        deployment: Deployment,
        resolved_mounts: Vec<ResolvedMount>,
    ) -> Deployment {
        super::lifecycle::apply(
            deployment,
            self.docker.clone(),
            resolved_mounts,
            self.intentional_shutdowns.clone(),
        )
        .await
    }

    async fn list_instances(&self, deployment_id: String, status: &str) -> Vec<String> {
        super::instances::list_instances(&self.docker, deployment_id, status).await
    }

    async fn list_instances_with_names(
        &self,
        deployment_id: String,
        status: &str,
    ) -> Vec<(String, String)> {
        super::instances::list_instances_with_names(&self.docker, deployment_id, status).await
    }

    async fn remove_instance(&self, instance_id: String) -> bool {
        self.intentional_shutdowns.mark(instance_id.clone()).await;
        super::container::remove_container_by_id(&self.docker, instance_id).await
    }

    async fn get_logs(
        &self,
        deployment_id: &str,
        tail: Option<&str>,
        since: Option<i32>,
        instance_filter: Option<&str>,
    ) -> Vec<Log> {
        let instances = self
            .list_instances_with_names(deployment_id.to_string(), "all")
            .await;
        let filtered = filter_instances(instances, instance_filter);

        let mut logs = Vec::new();
        for (instance_id, instance_name) in filtered {
            let instance_logs = super::logs::logs(&self.docker, instance_id, tail, since).await;
            for message in instance_logs {
                logs.push(Log {
                    instance: instance_name.clone(),
                    level: classify_log(&message),
                    timestamp: extract_date(&message),
                    message,
                });
            }
        }
        logs
    }

    async fn stream_logs(
        &self,
        deployment_id: &str,
        tail: Option<&str>,
        since: Option<i32>,
        instance_filter: Option<&str>,
    ) -> Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>> {
        let instances = self
            .list_instances_with_names(deployment_id.to_string(), "all")
            .await;
        let filtered = filter_instances(instances, instance_filter);

        if filtered.is_empty() {
            return Box::pin(stream::empty());
        }

        let mut streams: Vec<Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>>> =
            Vec::new();

        for (instance_id, instance_name) in filtered {
            let raw_stream =
                super::logs::logs_stream(self.docker.clone(), instance_id, tail, since).await;

            let mapped = raw_stream.map(move |line| {
                let log = Log {
                    instance: instance_name.clone(),
                    level: classify_log(&line),
                    timestamp: extract_date(&line),
                    message: line,
                };
                let json = serde_json::to_string(&log).unwrap_or_default();
                Ok(Event::default().data(json))
            });

            streams.push(Box::pin(mapped));
        }

        Box::pin(stream::select_all(streams))
    }

    async fn instance_address(&self, instance_id: &str) -> Option<std::net::IpAddr> {
        super::health_check::container_address(&self.docker, instance_id).await
    }

    async fn execute_command_probe(
        &self,
        instance_id: &str,
        command: &str,
    ) -> (HealthCheckStatus, Option<String>) {
        super::health_check::execute_command_check(&self.docker, instance_id, command).await
    }

    async fn get_instance_stats(&self, deployment_id: &str) -> Vec<InstanceStatsOutput> {
        let instances = self
            .list_instances_with_names(deployment_id.to_string(), "all")
            .await;
        let mut results = Vec::new();

        for (id, name) in instances {
            match super::stats::fetch_container_stats(&self.docker, &id).await {
                Ok(raw_stats) => {
                    let restart_count = super::stats::fetch_restart_count(&self.docker, &id).await;
                    results.push(InstanceStatsOutput {
                        instance_id: id.chars().take(12).collect(),
                        instance_name: name,
                        cpu_usage_percent: super::stats::compute_cpu_percent(&raw_stats),
                        memory: super::stats::compute_memory_stats(&raw_stats),
                        network: super::stats::compute_network_stats(&raw_stats),
                        disk_io: super::stats::compute_disk_io_stats(&raw_stats),
                        pids: super::stats::compute_pid_stats(&raw_stats),
                        restart_count,
                    });
                }
                Err(e) => {
                    log::warn!("Failed to get stats for instance {}: {}", id, e);
                }
            }
        }

        results
    }
}
