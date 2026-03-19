use crate::api::dto::stats::ContainerStatsOutput;
use crate::models::deployments::Deployment;
use crate::models::health_check::{HealthCheck, HealthCheckStatus};
use crate::runtime::docker;
use async_trait::async_trait;
use axum::response::sse::Event;
use bollard::Docker;
use futures::stream::{self, Stream, StreamExt};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use std::pin::Pin;
use std::sync::LazyLock;

pub struct Runtime {}

#[async_trait]
pub trait RuntimeInterface {
    async fn list_instances(&self) -> Vec<String>;
    async fn list_instances_with_names(&self) -> Vec<(String, String)>;
    async fn get_logs(
        &self,
        tail: Option<&str>,
        since: Option<i32>,
        container: Option<&str>,
    ) -> Vec<Log>;
    async fn stream_logs(
        &self,
        tail: Option<&str>,
        since: Option<i32>,
        container: Option<&str>,
    ) -> Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>>;
    async fn execute_health_check(
        &self,
        instance_id: &str,
        health_check: &HealthCheck,
    ) -> (HealthCheckStatus, Option<String>);
    async fn remove_instance(&self, instance_id: &str);
    async fn get_instance_stats(&self) -> Vec<ContainerStatsOutput>;
}

pub struct DockerRuntime {
    docker: Docker,
    deployment: Deployment,
}

impl Runtime {
    #[allow(clippy::new_ret_no_self)]
    pub fn new(
        deployment: Deployment,
    ) -> Result<Box<dyn RuntimeInterface + Send + Sync>, crate::runtime::error::RuntimeError> {
        let docker = docker::connect()?;
        Ok(Box::new(DockerRuntime { docker, deployment }))
    }
}

#[derive(Clone, Deserialize, Serialize, Debug)]
pub(crate) struct Log {
    pub(crate) instance: String,
    pub(crate) message: String,
    pub(crate) level: String,
    pub(crate) timestamp: Option<String>,
}

#[allow(clippy::if_same_then_else)]
fn classify_log(log: &str) -> String {
    if log.contains("[error]") {
        "error".to_string()
    } else if log.contains("[warning]") {
        "warning".to_string()
    } else if log.contains("[notice]") || log.contains("[info]") || log.contains("info:") {
        "info".to_string()
    } else {
        "info".to_string()
    }
}

static DATE_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\d{4}/\d{2}/\d{2} \d{2}:\d{2}:\d{2}").unwrap());

fn extract_date(log: &str) -> Option<String> {
    let date = DATE_REGEX.find(log).map(|d| d.as_str()).unwrap_or("");

    if date.is_empty() {
        return None;
    }

    Some(date.to_string())
}

#[async_trait]
impl RuntimeInterface for DockerRuntime {
    async fn list_instances(&self) -> Vec<String> {
        docker::list_instances(&self.docker, self.deployment.id.clone(), "all").await
    }

    async fn list_instances_with_names(&self) -> Vec<(String, String)> {
        docker::list_instances_with_names(&self.docker, self.deployment.id.clone(), "all").await
    }

    async fn get_logs(
        &self,
        tail: Option<&str>,
        since: Option<i32>,
        container: Option<&str>,
    ) -> Vec<Log> {
        let mut logs = vec![];

        let instances = self.list_instances_with_names().await;

        let filtered_instances: Vec<(String, String)> = if let Some(filter) = container {
            instances
                .into_iter()
                .filter(|(id, name)| id.starts_with(filter) || name.contains(filter))
                .collect()
        } else {
            instances
        };

        for (instance_id, instance_name) in filtered_instances {
            let instance_logs: Vec<String> =
                docker::logs(&self.docker, instance_id.clone(), tail, since).await;
            for message in instance_logs {
                let log = Log {
                    instance: instance_name.clone(),
                    level: classify_log(&message),
                    timestamp: extract_date(&message),
                    message,
                };
                logs.push(log);
            }
        }

        logs
    }

    async fn stream_logs(
        &self,
        tail: Option<&str>,
        since: Option<i32>,
        container: Option<&str>,
    ) -> Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>> {
        let instances = self.list_instances_with_names().await;

        let filtered_instances: Vec<(String, String)> = if let Some(filter) = container {
            instances
                .into_iter()
                .filter(|(id, name)| id.starts_with(filter) || name.contains(filter))
                .collect()
        } else {
            instances
        };

        if filtered_instances.is_empty() {
            return Box::pin(stream::empty());
        }

        let mut streams: Vec<Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>>> =
            Vec::new();

        for (instance_id, instance_name) in filtered_instances {
            let raw_stream =
                docker::logs_stream(self.docker.clone(), instance_id.clone(), tail, since).await;

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

    async fn execute_health_check(
        &self,
        instance_id: &str,
        health_check: &HealthCheck,
    ) -> (HealthCheckStatus, Option<String>) {
        docker::execute_health_check_for_instance(
            &self.docker,
            instance_id.to_string(),
            health_check.clone(),
        )
        .await
    }

    async fn remove_instance(&self, instance_id: &str) {
        docker::remove_container_by_id(&self.docker, instance_id.to_string()).await;
    }

    async fn get_instance_stats(&self) -> Vec<ContainerStatsOutput> {
        let instances = self.list_instances_with_names().await;
        let mut results = Vec::new();

        for (container_id, container_name) in instances {
            match docker::fetch_container_stats(&self.docker, &container_id).await {
                Ok(raw_stats) => {
                    let restart_count =
                        docker::fetch_restart_count(&self.docker, &container_id).await;
                    results.push(ContainerStatsOutput {
                        container_id: container_id.chars().take(12).collect(),
                        container_name,
                        cpu_usage_percent: docker::compute_cpu_percent(&raw_stats),
                        memory: docker::compute_memory_stats(&raw_stats),
                        network: docker::compute_network_stats(&raw_stats),
                        disk_io: docker::compute_disk_io_stats(&raw_stats),
                        pids: docker::compute_pid_stats(&raw_stats),
                        restart_count,
                    });
                }
                Err(e) => {
                    log::warn!("Failed to get stats for container {}: {}", container_id, e);
                }
            }
        }

        results
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_log() {
        assert_eq!(classify_log("[info] This is an info log"), "info");
        assert_eq!(classify_log("[error] This is an error log"), "error");
        assert_eq!(classify_log("[warning] This is a warning log"), "warning");
        assert_eq!(classify_log("[notice] This is a notice log"), "info");
        assert_eq!(classify_log("info: This is a notice log"), "info");
        assert_eq!(classify_log("Coucou"), "info");
    }

    #[test]
    fn test_extract_date() {
        assert_eq!(
            extract_date("2021/08/10 12:00:00 [info] This is an info log"),
            Some("2021/08/10 12:00:00".to_string())
        );
        assert_eq!(extract_date("[info] This is an info log"), None);
    }
}
