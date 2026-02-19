use bollard::Docker;
use crate::models::deployments::Deployment;
use crate::runtime::docker;
use async_trait::async_trait;
use axum::response::sse::Event;
use futures::stream::{self, Stream, StreamExt};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use std::pin::Pin;
use crate::models::health_check::{HealthCheck, HealthCheckStatus};

pub struct Runtime {
}

#[async_trait]
pub trait RuntimeInterface {
    async fn list_instances(&self) -> Vec<String>;
    async fn list_instances_with_names(&self) -> Vec<(String, String)>;
    async fn get_logs(&self, tail: Option<&str>, since: Option<i32>, container: Option<&str>) -> Vec<Log>;
    async fn stream_logs(&self, tail: Option<&str>, since: Option<i32>, container: Option<&str>) -> Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>>;
    async fn execute_health_check(&self, instance_id: &str, health_check: &HealthCheck) -> (HealthCheckStatus, Option<String>);
    async fn remove_instance(&self, instance_id: &str);
}

pub struct DockerRuntime {
    docker: Docker,
    deployment: Deployment,
}

impl Runtime {
    pub fn new(deployment: Deployment) -> Box<dyn RuntimeInterface + Send + Sync> {
        let docker = docker::connect().expect("Failed to connect to Docker");
        Box::new(DockerRuntime { docker, deployment })
    }
}

#[derive(Clone, Deserialize, Serialize, Debug)]
pub(crate) struct Log {
    pub(crate) instance: String,
    pub(crate) message: String,
    pub(crate) level: String,
    pub(crate) timestamp: Option<String>
}

fn classify_log(log: String) -> String {
    return if log.contains("[error]") {
        "error".to_string()
    } else if log.contains("[warning]") {
        "warning".to_string()
    } else if log.contains("[notice]") || log.contains("[info]") || log.contains("info:") {
        "info".to_string()
    } else {
        "info".to_string()
    }
}

fn extract_date(log: String) -> Option<String> {
    let date_regex = Regex::new(r"\d{4}/\d{2}/\d{2} \d{2}:\d{2}:\d{2}").unwrap();
    let date = date_regex.find(&*log).map(|d| d.as_str()).unwrap_or("");

    if date == "" {
        return None;
    }

    return Some(date.to_string());
}

#[async_trait]
impl RuntimeInterface for DockerRuntime {
    async fn list_instances(&self) -> Vec<String> {
        docker::list_instances(&self.docker, self.deployment.id.clone(), "all").await
    }

    async fn list_instances_with_names(&self) -> Vec<(String, String)> {
        docker::list_instances_with_names(&self.docker, self.deployment.id.clone(), "all").await
    }

    async fn get_logs(&self, tail: Option<&str>, since: Option<i32>, container: Option<&str>) -> Vec<Log> {
        let mut logs = vec![];

        let instances = self.list_instances_with_names().await;

        let filtered_instances: Vec<(String, String)> = if let Some(filter) = container {
            instances.into_iter()
                .filter(|(id, name)| id.starts_with(filter) || name.contains(filter))
                .collect()
        } else {
            instances
        };

        for (instance_id, instance_name) in filtered_instances {
            let instance_logs: Vec<String> = docker::logs(&self.docker, instance_id.clone(), tail, since).await;
            for message in instance_logs {
                let log = Log {
                    instance: instance_name.clone(),
                    message: message.clone(),
                    level: classify_log(message.clone()),
                    timestamp: extract_date(message),
                };
                logs.push(log);
            }
        }

        logs
    }

    async fn stream_logs(&self, tail: Option<&str>, since: Option<i32>, container: Option<&str>) -> Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>> {
        let instances = self.list_instances_with_names().await;

        let filtered_instances: Vec<(String, String)> = if let Some(filter) = container {
            instances.into_iter()
                .filter(|(id, name)| id.starts_with(filter) || name.contains(filter))
                .collect()
        } else {
            instances
        };

        if filtered_instances.is_empty() {
            return Box::pin(stream::empty());
        }

        let mut streams: Vec<Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>>> = Vec::new();

        for (instance_id, instance_name) in filtered_instances {
            let raw_stream = docker::logs_stream(self.docker.clone(), instance_id.clone(), tail, since).await;

            let mapped = raw_stream.map(move |line| {
                let log = Log {
                    instance: instance_name.clone(),
                    message: line.clone(),
                    level: classify_log(line.clone()),
                    timestamp: extract_date(line),
                };
                let json = serde_json::to_string(&log).unwrap_or_default();
                Ok(Event::default().data(json))
            });

            streams.push(Box::pin(mapped));
        }

        Box::pin(stream::select_all(streams))
    }

    async fn execute_health_check(&self, instance_id: &str, health_check: &HealthCheck) -> (HealthCheckStatus, Option<String>) {
        docker::execute_health_check_for_instance(&self.docker, instance_id.to_string(), health_check.clone()).await
    }

    async fn remove_instance(&self, instance_id: &str) {
        docker::remove_container_by_id(&self.docker, instance_id.to_string()).await;
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_log() {
        let log = "[info] This is an info log".to_string();
        assert_eq!(classify_log(log), "info".to_string());

        let log = "[error] This is an error log".to_string();
        assert_eq!(classify_log(log), "error".to_string());

        let log = "[warning] This is a warning log".to_string();
        assert_eq!(classify_log(log), "warning".to_string());

        let log = "[notice] This is a notice log".to_string();
        assert_eq!(classify_log(log), "info".to_string());

        let log = "info: This is a notice log".to_string();
        assert_eq!(classify_log(log), "info".to_string());

        let log = "Coucou".to_string();
        assert_eq!(classify_log(log), "info".to_string());
    }

    #[test]
    fn test_extract_date() {
        let log = "2021/08/10 12:00:00 [info] This is an info log".to_string();
        assert_eq!(extract_date(log), Some("2021/08/10 12:00:00".to_string()));

        let log = "[info] This is an info log".to_string();
        assert_eq!(extract_date(log), None);
    }
}
