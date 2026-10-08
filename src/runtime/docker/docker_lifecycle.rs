use crate::api::dto::stats::InstanceStatsOutput;
use crate::hypervisor::error::RuntimeError;
use crate::hypervisor::instance_driver::{InstanceDriver, Observation};
use crate::hypervisor::lifecycle_trait::{
    ExecError, ExecRequest, ExecSession, Log, RuntimeLifecycle, classify_log, extract_date,
};
use crate::models::deployments::Deployment;
use crate::models::health_check::HealthCheckStatus;
use crate::models::restart_state::{LOGS_TAIL_LINES, Termination, logs_tail};
use crate::models::volume::{ReloadSignal, ResolvedMount};
use crate::runtime::registry_auth::HostAuthSettings;
use crate::scheduler::intentional_shutdowns::IntentionalShutdowns;
use async_trait::async_trait;
use axum::response::sse::Event;
use bollard::Docker;
use bollard::query_parameters::{InspectContainerOptions, KillContainerOptionsBuilder};
use chrono::{DateTime, Utc};
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
    /// Server-side host registry auth settings for this runtime, from
    /// `[server.runtime.docker]` / `[server.runtime.podman]`.
    host_auth: HostAuthSettings,
}

impl DockerLifecycle {
    /// Crash counting is reconcile-driven: the scheduler observes exited
    /// containers through the instance driver and owns `restart_count`, so
    /// Docker and Podman back off the same way. The Docker event
    /// listener is observational only (it logs crash causes, never the count).
    pub fn new(
        docker: Docker,
        intentional_shutdowns: IntentionalShutdowns,
        host_auth: HostAuthSettings,
    ) -> Self {
        Self {
            docker,
            intentional_shutdowns,
            host_auth,
        }
    }

    /// Podman registers under its own key but shares the Docker-compatible
    /// lifecycle; crash detection is identical, so this delegates to `new`.
    pub fn new_podman(
        docker: Docker,
        intentional_shutdowns: IntentionalShutdowns,
        host_auth: HostAuthSettings,
    ) -> Self {
        Self::new(docker, intentional_shutdowns, host_auth)
    }
}

#[async_trait]
impl InstanceDriver for DockerLifecycle {
    async fn observe(&self, deployment: &Deployment) -> Observation {
        let (running, ended) =
            super::instances::list_live_and_ended(&self.docker, &deployment.id).await;

        let mut terminated = Vec::new();
        for instance_id in ended {
            // Stopped by Ring itself (scale-down, rolling drain, health check):
            // not a failure, nothing to report.
            if self.intentional_shutdowns.take(&instance_id).await {
                super::container::remove_container(self.docker.clone(), instance_id).await;
                continue;
            }
            terminated.push(self.termination(instance_id).await);
        }
        terminated.sort_by_key(|t| t.finished_at);

        Observation {
            running,
            terminated,
        }
    }

    async fn start_instance(
        &self,
        deployment: &mut Deployment,
        resolved_mounts: &[ResolvedMount],
    ) -> Result<(), RuntimeError> {
        super::container::create_container(
            deployment,
            &self.docker,
            resolved_mounts,
            &self.host_auth,
        )
        .await
    }

    async fn stop_instance(&self, instance_id: &str) -> bool {
        self.intentional_shutdowns
            .mark(instance_id.to_string())
            .await;
        super::container::remove_container_by_id(&self.docker, instance_id.to_string()).await
    }

    async fn discard_instance(&self, instance_id: &str) {
        super::container::remove_container(self.docker.clone(), instance_id.to_string()).await;
    }
}

impl DockerLifecycle {
    /// How an ended container finished: its exit code, when, and the end of
    /// its output, read before the container is removed with them.
    async fn termination(&self, instance_id: String) -> Termination {
        let state = self
            .docker
            .inspect_container(&instance_id, None::<InspectContainerOptions>)
            .await
            .ok()
            .and_then(|details| details.state);
        let exit_code = state.as_ref().and_then(|s| s.exit_code);
        // A container that never ran reports the zero time; fall back to now.
        let finished_at = state
            .as_ref()
            .and_then(|s| s.finished_at.as_deref())
            .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
            .map(|t| t.with_timezone(&Utc))
            .filter(|t| t.timestamp() > 0)
            .unwrap_or_else(Utc::now);
        let tail = (LOGS_TAIL_LINES).to_string();
        let lines = super::logs::logs(&self.docker, instance_id.clone(), Some(&tail), None).await;

        Termination {
            instance_id,
            exit_code,
            finished_at,
            logs_tail: logs_tail(&lines),
        }
    }
}

#[async_trait]
impl RuntimeLifecycle for DockerLifecycle {
    fn instance_driver(&self) -> Option<&dyn InstanceDriver> {
        Some(self)
    }

    async fn apply(
        &self,
        deployment: Deployment,
        _resolved_mounts: Vec<ResolvedMount>,
    ) -> Deployment {
        super::lifecycle::apply(
            deployment,
            self.docker.clone(),
            self.intentional_shutdowns.clone(),
        )
        .await
    }

    async fn list_instances(&self, deployment_id: String, status: &str) -> Vec<String> {
        super::instances::list_instances(&self.docker, deployment_id, status).await
    }

    async fn list_running_instances_grouped(
        &self,
        deployment_ids: &[String],
    ) -> std::collections::HashMap<String, Vec<String>> {
        super::instances::list_running_instances_grouped(&self.docker, deployment_ids).await
    }

    async fn list_instances_with_names(
        &self,
        deployment_id: String,
        status: &str,
    ) -> Vec<(String, String)> {
        super::instances::list_instances_with_names(&self.docker, deployment_id, status).await
    }

    async fn instance_started_at(&self, instance_id: &str) -> Option<DateTime<Utc>> {
        let details = self
            .docker
            .inspect_container(instance_id, None::<InspectContainerOptions>)
            .await
            .ok()?;
        let started_at = details.state?.started_at?;
        DateTime::parse_from_rfc3339(&started_at)
            .ok()
            .map(|t| t.with_timezone(&Utc))
    }

    async fn signal_instances(
        &self,
        deployment_id: &str,
        signal: ReloadSignal,
    ) -> Result<usize, RuntimeError> {
        let instances = self
            .list_instances(deployment_id.to_string(), "running")
            .await;

        let mut signalled = 0;
        let mut failures = Vec::new();
        for instance_id in instances {
            // The signal must always be explicit: the kill endpoint defaults
            // to SIGKILL.
            let options = KillContainerOptionsBuilder::new()
                .signal(signal.as_str())
                .build();
            match self
                .docker
                .kill_container(&instance_id, Some(options))
                .await
            {
                Ok(()) => signalled += 1,
                Err(e) => failures.push(format!("{}: {}", instance_id, e)),
            }
        }

        if failures.is_empty() {
            Ok(signalled)
        } else {
            Err(RuntimeError::Other(format!(
                "{} not delivered to {} of {} instances ({})",
                signal.as_str(),
                failures.len(),
                failures.len() + signalled,
                failures.join("; ")
            )))
        }
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

    async fn exec(
        &self,
        instance_id: &str,
        request: ExecRequest,
    ) -> Result<ExecSession, ExecError> {
        super::exec::exec(&self.docker, instance_id, request).await
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
                    warn!("Failed to get stats for instance {}: {}", id, e);
                }
            }
        }

        results
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn driver_test_worker(id: &str, command: &[&str]) -> Deployment {
        use crate::models::deployments::DeploymentStatus;
        use std::collections::HashMap;

        Deployment {
            id: id.to_string(),
            created_at: Utc::now().to_string(),
            updated_at: None,
            status: DeploymentStatus::Pending,
            restart_count: 0,
            namespace: "ring-driver-test".to_string(),
            name: id.to_string(),
            image: crate::runtime::docker::test_support::TEST_IMAGE.to_string(),
            config: None,
            runtime: "docker".to_string(),
            kind: "worker".to_string(),
            replicas: 1,
            command: command.iter().map(|s| s.to_string()).collect(),
            instances: vec![],
            labels: HashMap::new(),
            environment: HashMap::new(),
            volumes: "[]".to_string(),
            health_checks: vec![],
            resources: None,
            autoscale: None,
            desired_replicas: None,
            image_digest: None,
            ports: vec![],
            pending_events: vec![],
            parent_id: None,
            network: None,
            restart: None,
        }
    }

    /// The driver reports an instance that exited with its code and output,
    /// keeps reporting it until it is discarded, and does not report one it
    /// stopped itself.
    #[tokio::test]
    async fn driver_reports_exits_but_not_its_own_stops() {
        use crate::runtime::docker::test_support::{daemon, ensure_image};

        let Some(docker) = daemon().await else {
            eprintln!("skipping: no Docker daemon");
            return;
        };
        if !ensure_image(&docker).await {
            eprintln!("skipping: could not obtain the test image");
            return;
        }
        let driver = DockerLifecycle::new(
            docker.clone(),
            IntentionalShutdowns::new(),
            HostAuthSettings::default(),
        );

        // An instance that exits on its own.
        let mut crasher = driver_test_worker(
            "ring-driver-crasher",
            &["sh", "-c", "echo going down; exit 3"],
        );
        driver.start_instance(&mut crasher, &[]).await.unwrap();
        let mut observed = Observation::default();
        for _ in 0..50 {
            observed = driver.observe(&crasher).await;
            if !observed.terminated.is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        let ended = observed.terminated.first().cloned();
        if let Some(t) = &ended {
            driver.discard_instance(&t.instance_id).await;
        }
        let after_discard = driver.observe(&crasher).await;

        // An instance Ring stops itself.
        let mut sleeper = driver_test_worker("ring-driver-sleeper", &["sleep", "60"]);
        driver.start_instance(&mut sleeper, &[]).await.unwrap();
        let running = driver.observe(&sleeper).await;
        let stopped = driver.stop_instance(&sleeper.instances[0]).await;
        let after_stop = driver.observe(&sleeper).await;

        let ended = ended.expect("the crashed instance is reported");
        assert_eq!(ended.exit_code, Some(3));
        assert_eq!(ended.logs_tail.as_deref(), Some("going down"));
        assert_eq!(after_discard, Observation::default());
        assert_eq!(running.running.len(), 1);
        assert!(stopped);
        assert_eq!(after_stop, Observation::default());
    }

    /// The start time comes from the daemon's own format, so only a real
    /// daemon can tell whether it parses.
    #[tokio::test]
    async fn instance_started_at_reads_the_daemon_start_time() {
        use crate::runtime::docker::test_support::{
            TEST_IMAGE, daemon, ensure_image, remove, start_container,
        };

        let Some(docker) = daemon().await else {
            eprintln!("skipping: no Docker daemon");
            return;
        };
        if !ensure_image(&docker).await {
            eprintln!("skipping: could not obtain {TEST_IMAGE}");
            return;
        }

        let before = chrono::Utc::now() - chrono::Duration::seconds(5);
        let id = start_container(&docker, "ring-started-at", true).await;
        let lifecycle = DockerLifecycle::new(
            docker.clone(),
            IntentionalShutdowns::new(),
            HostAuthSettings::default(),
        );

        let started_at = lifecycle.instance_started_at(&id).await;
        let unknown = lifecycle.instance_started_at("no-such-container").await;
        remove(&docker, &id).await;

        let started_at = started_at.expect("a running container has a start time");
        assert!(
            started_at >= before && started_at <= chrono::Utc::now(),
            "start time {started_at} is not around now"
        );
        assert_eq!(unknown, None);
    }
}
