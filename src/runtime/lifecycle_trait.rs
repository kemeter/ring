use crate::api::dto::stats::InstanceStatsOutput;
use crate::models::deployments::Deployment;
use crate::models::health_check::{HealthCheck, HealthCheckStatus};
use crate::models::volume::ResolvedMount;
use async_trait::async_trait;
use axum::response::sse::Event;
use futures::stream;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use std::net::IpAddr;
use std::pin::Pin;
use std::sync::LazyLock;

#[derive(Clone, Deserialize, Serialize, Debug)]
pub(crate) struct Log {
    pub(crate) instance: String,
    pub(crate) message: String,
    pub(crate) level: String,
    pub(crate) timestamp: Option<String>,
}

pub(crate) fn classify_log(log: &str) -> String {
    if log.contains("[error]") {
        "error".to_string()
    } else if log.contains("[warning]") {
        "warning".to_string()
    } else if log.contains("[notice]") || log.contains("[info]") || log.contains("info:") {
        "info".to_string()
    } else {
        "unknown".to_string()
    }
}

static DATE_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\d{4}/\d{2}/\d{2} \d{2}:\d{2}:\d{2}").unwrap());

pub(crate) fn extract_date(log: &str) -> Option<String> {
    let date = DATE_REGEX.find(log).map(|d| d.as_str()).unwrap_or("");
    if date.is_empty() {
        return None;
    }
    Some(date.to_string())
}

#[async_trait]
pub trait RuntimeLifecycle: Send + Sync {
    async fn apply(
        &self,
        deployment: Deployment,
        resolved_mounts: Vec<ResolvedMount>,
    ) -> Deployment;

    async fn list_instances(&self, deployment_id: String, status: &str) -> Vec<String>;

    /// Fallback: uses instance ID as display name. Override for runtimes
    /// that assign human-readable names (e.g. Docker container names).
    async fn list_instances_with_names(
        &self,
        deployment_id: String,
        status: &str,
    ) -> Vec<(String, String)> {
        self.list_instances(deployment_id, status)
            .await
            .into_iter()
            .map(|id| {
                let name = id.clone();
                (id, name)
            })
            .collect()
    }

    async fn remove_instance(&self, instance_id: String) -> bool;

    async fn get_logs(
        &self,
        _deployment_id: &str,
        _tail: Option<&str>,
        _since: Option<i32>,
        _instance_filter: Option<&str>,
    ) -> Vec<Log> {
        Vec::new()
    }

    async fn stream_logs(
        &self,
        _deployment_id: &str,
        _tail: Option<&str>,
        _since: Option<i32>,
        _instance_filter: Option<&str>,
    ) -> Pin<Box<dyn futures::Stream<Item = Result<Event, Infallible>> + Send>> {
        Box::pin(stream::empty())
    }

    /// Resolve the instance's reachable address for an external probe.
    ///
    /// Runtimes that expose their workloads on the host network or on a
    /// runtime-private network should override this so TCP/HTTP probes can
    /// reach the workload. The default returns `None`, which causes the
    /// default `execute_health_check` to fail any TCP/HTTP probe with a
    /// clear "could not resolve" message.
    async fn instance_address(&self, _instance_id: &str) -> Option<IpAddr> {
        None
    }

    /// Run a `command` health-check probe inside the instance.
    ///
    /// Container runtimes implement this via `docker exec` or equivalent.
    /// VM runtimes have no direct equivalent — supporting `command` requires
    /// an in-guest agent (vsock or SSH), so the default impl reports the
    /// limitation up front rather than silently appearing to work.
    async fn execute_command_probe(
        &self,
        _instance_id: &str,
        _command: &str,
    ) -> (HealthCheckStatus, Option<String>) {
        (
            HealthCheckStatus::Failed,
            Some("command health checks are not supported on this runtime".to_string()),
        )
    }

    /// Execute one health-check definition for one instance.
    ///
    /// The default impl orchestrates the three probe types via shared
    /// helpers: `tcp` and `http` probes go through `health_probes` once an
    /// IP has been resolved via [`Self::instance_address`]; `command`
    /// probes are delegated to [`Self::execute_command_probe`].
    ///
    /// A runtime overrides this only if it needs to deviate from the
    /// shared pipeline — for example, the Docker runtime keeps its own
    /// override because its IP-resolution path is interleaved with
    /// `bollard::inspect_container` calls that already exist there.
    async fn execute_health_check(
        &self,
        instance_id: &str,
        health_check: &HealthCheck,
    ) -> (HealthCheckStatus, Option<String>) {
        let timeout = match HealthCheck::parse_duration(health_check.timeout()) {
            Ok(d) => d,
            Err(e) => {
                return (
                    HealthCheckStatus::Failed,
                    Some(format!("Invalid timeout duration: {}", e)),
                );
            }
        };

        match health_check {
            HealthCheck::Tcp { port, .. } => {
                let Some(ip) = self.instance_address(instance_id).await else {
                    return (
                        HealthCheckStatus::Failed,
                        Some(format!(
                            "Could not resolve instance address for {}",
                            instance_id
                        )),
                    );
                };
                crate::runtime::health_probes::tcp_probe(ip, *port, timeout).await
            }
            HealthCheck::Http { url, .. } => {
                let Some(ip) = self.instance_address(instance_id).await else {
                    return (
                        HealthCheckStatus::Failed,
                        Some(format!(
                            "Could not resolve instance address for {}",
                            instance_id
                        )),
                    );
                };
                crate::runtime::health_probes::http_probe(ip, url, timeout).await
            }
            HealthCheck::Command { command, .. } => {
                self.execute_command_probe(instance_id, command).await
            }
        }
    }

    async fn get_instance_stats(&self, _deployment_id: &str) -> Vec<InstanceStatsOutput> {
        Vec::new()
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
        assert_eq!(classify_log("Coucou"), "unknown");
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
