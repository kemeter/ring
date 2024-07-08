use crate::models::deployments::Deployment;
use crate::runtime::docker;
use async_trait::async_trait;
use regex::Regex;
use serde::{Deserialize, Serialize};

pub struct Runtime {
}

#[async_trait]
pub trait RuntimeInterface {
    async fn list_instances(&self) -> Vec<String>;
    async fn get_logs(&self) -> Vec<Log>;
}

pub struct DockerRuntime {
    deployment: Deployment,
}

impl Runtime {
    pub fn new(deployment: Deployment) -> Box<dyn RuntimeInterface + Send + Sync> {
        Box::new(DockerRuntime { deployment })
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
        docker::list_instances(self.deployment.clone().id).await
    }

    async fn get_logs(&self) -> Vec<Log> {
        let mut logs = vec![];

        let instances = self.list_instances().await;

        for instance in instances {
            let instance_logs: Vec<String> = docker::logs(instance.clone()).await;
            for message in instance_logs {
                let log = Log {
                    instance: instance.clone(),
                    message: message.clone(),
                    level: classify_log(message.clone()),
                    timestamp: extract_date(message),
                };
                logs.push(log);
            }
        }

        logs
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
