//! A runtime that was unreachable at startup, reconnected once it answers.
//!
//! Startup is best-effort: an enabled runtime whose daemon does not answer is
//! not fatal, so the others keep working. It used to be left out entirely:
//! its deployments were skipped by the scheduler without a word until Ring
//! was restarted, and `/healthz` still said `UP`. After a reboot where Docker
//! came up a little after Ring, every Docker workload stayed down.
//!
//! This stands in for such a runtime under its name. Until the daemon
//! answers, each call tries to connect again, at most every
//! [`RETRY_EVERY`], and otherwise answers as an unreachable runtime does:
//! `observe` fails, so the scheduler skips the deployment with a warning
//! rather than acting on an empty view. Once connected, everything goes to
//! the real runtime.

use crate::api::dto::stats::InstanceStatsOutput;
use crate::hypervisor::error::RuntimeError;
use crate::hypervisor::instance_driver::{InstanceDriver, Observation};
use crate::hypervisor::lifecycle_trait::{
    ExecError, ExecRequest, ExecSession, Log, RuntimeLifecycle, VolumeRemoval,
};
use crate::models::deployments::Deployment;
use crate::models::health_check::{HealthCheck, HealthCheckStatus};
use crate::models::volume::{ReloadSignal, ResolvedMount};
use async_trait::async_trait;
use axum::response::sse::Event;
use chrono::{DateTime, Utc};
use futures::future::BoxFuture;
use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::net::IpAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use tracing::{info, warn};

/// How often a call tries to reach an unreachable runtime again.
pub(crate) const RETRY_EVERY: Duration = Duration::from_secs(30);

type Connect =
    Box<dyn Fn() -> BoxFuture<'static, Result<Arc<dyn RuntimeLifecycle>, String>> + Send + Sync>;

pub(crate) struct ReconnectingRuntime {
    name: String,
    connect: Connect,
    inner: RwLock<Option<Arc<dyn RuntimeLifecycle>>>,
    last_attempt: Mutex<Option<Instant>>,
    retry_every: Duration,
}

impl ReconnectingRuntime {
    pub(crate) fn new(name: &str, connect: Connect) -> Self {
        Self::with_retry(name, connect, RETRY_EVERY)
    }

    fn with_retry(name: &str, connect: Connect, retry_every: Duration) -> Self {
        Self {
            name: name.to_string(),
            connect,
            inner: RwLock::new(None),
            last_attempt: Mutex::new(None),
            retry_every,
        }
    }

    /// The runtime, connecting first if it is time to try again.
    async fn runtime(&self) -> Option<Arc<dyn RuntimeLifecycle>> {
        if let Some(runtime) = self.inner.read().unwrap_or_else(|e| e.into_inner()).clone() {
            return Some(runtime);
        }
        {
            let mut last = self.last_attempt.lock().unwrap_or_else(|e| e.into_inner());
            if last.is_some_and(|at| at.elapsed() < self.retry_every) {
                return None;
            }
            *last = Some(Instant::now());
        }
        match (self.connect)().await {
            Ok(runtime) => {
                info!(
                    "{} runtime is reachable again, its deployments are reconciled",
                    self.name
                );
                *self.inner.write().unwrap_or_else(|e| e.into_inner()) = Some(runtime.clone());
                Some(runtime)
            }
            Err(e) => {
                warn!(
                    "{} runtime still unreachable, its deployments wait: {}",
                    self.name, e
                );
                None
            }
        }
    }

    /// Keep trying until the daemon answers, so the runtime comes back even
    /// when nothing calls it in the meantime (no deployment uses it yet), and
    /// `/healthz` reports it as soon as it does.
    pub(crate) fn spawn_reconnect_loop(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let runtime = self.clone();
        tokio::spawn(async move {
            while !runtime.is_connected() {
                runtime.runtime().await;
                tokio::time::sleep(runtime.retry_every).await;
            }
        })
    }

    fn unreachable(&self) -> RuntimeError {
        RuntimeError::Other(format!("the {} runtime is unreachable", self.name))
    }
}

#[async_trait]
impl InstanceDriver for ReconnectingRuntime {
    async fn observe(&self, deployment: &Deployment) -> Result<Observation, RuntimeError> {
        match self.runtime().await {
            Some(runtime) => runtime.observe(deployment).await,
            None => Err(self.unreachable()),
        }
    }

    async fn start_instance(
        &self,
        deployment: &mut Deployment,
        resolved_mounts: &[ResolvedMount],
    ) -> Result<(), RuntimeError> {
        match self.runtime().await {
            Some(runtime) => runtime.start_instance(deployment, resolved_mounts).await,
            None => Err(self.unreachable()),
        }
    }

    async fn stop_instance(&self, instance_id: &str) -> bool {
        match self.runtime().await {
            Some(runtime) => runtime.stop_instance(instance_id).await,
            None => false,
        }
    }

    async fn discard_instance(&self, instance_id: &str) {
        if let Some(runtime) = self.runtime().await {
            runtime.discard_instance(instance_id).await;
        }
    }

    async fn retire_instance(&self, instance_id: &str) {
        if let Some(runtime) = self.runtime().await {
            runtime.retire_instance(instance_id).await;
        }
    }

    async fn teardown(&self, deployment: &mut Deployment) -> Result<(), RuntimeError> {
        match self.runtime().await {
            Some(runtime) => runtime.teardown(deployment).await,
            None => Err(self.unreachable()),
        }
    }
}

#[async_trait]
impl RuntimeLifecycle for ReconnectingRuntime {
    fn is_connected(&self) -> bool {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
    }

    async fn remove_volume(&self, name: &str, namespace: &str) -> Result<VolumeRemoval, String> {
        match self.runtime().await {
            Some(runtime) => runtime.remove_volume(name, namespace).await,
            None => Err(self.unreachable().to_string()),
        }
    }

    async fn list_instances(&self, deployment_id: String, status: &str) -> Vec<String> {
        match self.runtime().await {
            Some(runtime) => runtime.list_instances(deployment_id, status).await,
            None => Vec::new(),
        }
    }

    async fn try_list_instances(
        &self,
        deployment_id: String,
        status: &str,
    ) -> Result<Vec<String>, RuntimeError> {
        match self.runtime().await {
            Some(runtime) => runtime.try_list_instances(deployment_id, status).await,
            None => Err(self.unreachable()),
        }
    }

    async fn list_running_instances_grouped(
        &self,
        deployment_ids: &[String],
    ) -> HashMap<String, Vec<String>> {
        match self.runtime().await {
            Some(runtime) => runtime.list_running_instances_grouped(deployment_ids).await,
            None => HashMap::new(),
        }
    }

    async fn list_instances_with_names(
        &self,
        deployment_id: String,
        status: &str,
    ) -> Vec<(String, String)> {
        match self.runtime().await {
            Some(runtime) => {
                runtime
                    .list_instances_with_names(deployment_id, status)
                    .await
            }
            None => Vec::new(),
        }
    }

    async fn remove_instance(&self, instance_id: String) -> bool {
        match self.runtime().await {
            Some(runtime) => runtime.remove_instance(instance_id).await,
            None => false,
        }
    }

    async fn get_logs(
        &self,
        deployment_id: &str,
        tail: Option<&str>,
        since: Option<i32>,
        instance_filter: Option<&str>,
    ) -> Vec<Log> {
        match self.runtime().await {
            Some(runtime) => {
                runtime
                    .get_logs(deployment_id, tail, since, instance_filter)
                    .await
            }
            None => Vec::new(),
        }
    }

    async fn stream_logs(
        &self,
        deployment_id: &str,
        tail: Option<&str>,
        since: Option<i32>,
        instance_filter: Option<&str>,
    ) -> Pin<Box<dyn futures::Stream<Item = Result<Event, Infallible>> + Send>> {
        match self.runtime().await {
            Some(runtime) => {
                runtime
                    .stream_logs(deployment_id, tail, since, instance_filter)
                    .await
            }
            None => Box::pin(futures::stream::empty()),
        }
    }

    async fn instance_address(&self, instance_id: &str) -> Option<IpAddr> {
        self.runtime().await?.instance_address(instance_id).await
    }

    async fn execute_command_probe(
        &self,
        instance_id: &str,
        command: &str,
    ) -> (HealthCheckStatus, Option<String>) {
        match self.runtime().await {
            Some(runtime) => runtime.execute_command_probe(instance_id, command).await,
            None => (
                HealthCheckStatus::Failed,
                Some(self.unreachable().to_string()),
            ),
        }
    }

    async fn exec(
        &self,
        instance_id: &str,
        request: ExecRequest,
    ) -> Result<ExecSession, ExecError> {
        match self.runtime().await {
            Some(runtime) => runtime.exec(instance_id, request).await,
            None => Err(ExecError::Failed(self.unreachable().to_string())),
        }
    }

    async fn instance_started_at(&self, instance_id: &str) -> Option<DateTime<Utc>> {
        self.runtime().await?.instance_started_at(instance_id).await
    }

    async fn signal_instances(
        &self,
        deployment_id: &str,
        signal: ReloadSignal,
    ) -> Result<usize, RuntimeError> {
        match self.runtime().await {
            Some(runtime) => runtime.signal_instances(deployment_id, signal).await,
            None => Err(self.unreachable()),
        }
    }

    async fn execute_health_check(
        &self,
        instance_id: &str,
        health_check: &HealthCheck,
    ) -> (HealthCheckStatus, Option<String>) {
        match self.runtime().await {
            Some(runtime) => {
                runtime
                    .execute_health_check(instance_id, health_check)
                    .await
            }
            None => (
                HealthCheckStatus::Failed,
                Some(self.unreachable().to_string()),
            ),
        }
    }

    async fn get_instance_stats(&self, deployment_id: &str) -> Vec<InstanceStatsOutput> {
        match self.runtime().await {
            Some(runtime) => runtime.get_instance_stats(deployment_id).await,
            None => Vec::new(),
        }
    }

    async fn release_namespaces(&self, active: &HashSet<String>) {
        if let Some(runtime) = self.runtime().await {
            runtime.release_namespaces(active).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hypervisor::mock::MockRuntime;
    use futures::FutureExt;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn deployment() -> Deployment {
        serde_json::from_value(serde_json::json!({
            "id": "d1", "created_at": "", "status": "running", "restart_count": 0,
            "namespace": "ns", "name": "web", "image": "nginx", "runtime": "docker",
            "kind": "worker", "replicas": 1, "command": [], "labels": {},
            "environment": {}, "volumes": "[]"
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn an_unreachable_runtime_is_retried_then_used() {
        let attempts = Arc::new(AtomicU32::new(0));
        let up = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (a, u) = (attempts.clone(), up.clone());
        let runtime = ReconnectingRuntime::with_retry(
            "docker",
            Box::new(move || {
                a.fetch_add(1, Ordering::SeqCst);
                let up = u.load(Ordering::SeqCst);
                async move {
                    if up {
                        Ok(Arc::new(MockRuntime::healthy()) as Arc<dyn RuntimeLifecycle>)
                    } else {
                        Err("connection refused".to_string())
                    }
                }
                .boxed()
            }),
            Duration::from_millis(20),
        );

        // Down: observing fails rather than reporting no instance.
        assert!(runtime.observe(&deployment()).await.is_err());
        assert!(!runtime.is_connected());
        // Not retried again right away.
        assert!(runtime.observe(&deployment()).await.is_err());
        assert_eq!(attempts.load(Ordering::SeqCst), 1);

        // The daemon comes up: the next attempt connects, and sticks.
        up.store(true, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(runtime.observe(&deployment()).await.is_ok());
        assert!(runtime.is_connected());
        assert!(runtime.observe(&deployment()).await.is_ok());
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    /// The rolling drain deletes a parent it sees empty: an unreachable
    /// runtime must answer an error there, not an empty list.
    #[tokio::test]
    async fn an_unreachable_runtime_fails_a_strict_listing() {
        let runtime = ReconnectingRuntime::with_retry(
            "docker",
            Box::new(|| async { Err("connection refused".to_string()) }.boxed()),
            Duration::from_secs(60),
        );
        assert!(
            runtime
                .try_list_instances("d1".to_string(), "running")
                .await
                .is_err()
        );
    }
}
