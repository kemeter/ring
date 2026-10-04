use crate::api::dto::stats::InstanceStatsOutput;
use crate::hypervisor::error::RuntimeError;
use crate::hypervisor::lifecycle_trait::{Log, RuntimeLifecycle};
use crate::models::deployments::Deployment;
use crate::models::health_check::{HealthCheck, HealthCheckStatus};
use crate::models::volume::{ReloadSignal, ResolvedMount};
use async_trait::async_trait;
use axum::response::sse::Event;
use std::convert::Infallible;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

pub(crate) struct MockRuntime {
    health_check_result: (HealthCheckStatus, Option<String>),
    instance_stats: Vec<InstanceStatsOutput>,
    signals: Arc<Mutex<Vec<(String, ReloadSignal)>>>,
    started_at: std::collections::HashMap<String, chrono::DateTime<chrono::Utc>>,
    removed: Arc<Mutex<Vec<String>>>,
}

impl MockRuntime {
    pub(crate) fn healthy() -> Self {
        Self {
            health_check_result: (HealthCheckStatus::Success, None),
            instance_stats: Vec::new(),
            signals: Arc::default(),
            started_at: std::collections::HashMap::new(),
            removed: Arc::default(),
        }
    }

    pub(crate) fn unhealthy(message: &str) -> Self {
        Self {
            health_check_result: (HealthCheckStatus::Failed, Some(message.to_string())),
            instance_stats: Vec::new(),
            signals: Arc::default(),
            started_at: std::collections::HashMap::new(),
            removed: Arc::default(),
        }
    }

    /// Seed the stats this mock returns from `get_instance_stats`, so the
    /// stats-cache refresh path can be exercised with deterministic numbers.
    pub(crate) fn with_instance_stats(mut self, stats: Vec<InstanceStatsOutput>) -> Self {
        self.instance_stats = stats;
        self
    }

    /// Report `started_at` as the start of `instance_id`.
    pub(crate) fn with_started_at(
        mut self,
        instance_id: &str,
        started_at: chrono::DateTime<chrono::Utc>,
    ) -> Self {
        self.started_at.insert(instance_id.to_string(), started_at);
        self
    }

    /// Every instance id passed to `remove_instance`.
    pub(crate) fn removed_log(&self) -> Arc<Mutex<Vec<String>>> {
        self.removed.clone()
    }

    /// Every `(deployment_id, signal)` passed to `signal_instances`, shared so
    /// a test can keep a handle after the mock moves into a runtime map.
    pub(crate) fn signal_log(&self) -> Arc<Mutex<Vec<(String, ReloadSignal)>>> {
        self.signals.clone()
    }
}

#[async_trait]
impl RuntimeLifecycle for MockRuntime {
    async fn apply(
        &self,
        deployment: Deployment,
        _resolved_mounts: Vec<ResolvedMount>,
    ) -> Deployment {
        deployment
    }

    async fn list_instances(&self, _deployment_id: String, _status: &str) -> Vec<String> {
        Vec::new()
    }

    async fn remove_instance(&self, instance_id: String) -> bool {
        self.removed.lock().unwrap().push(instance_id);
        true
    }

    async fn instance_started_at(
        &self,
        instance_id: &str,
    ) -> Option<chrono::DateTime<chrono::Utc>> {
        self.started_at.get(instance_id).copied()
    }

    async fn signal_instances(
        &self,
        deployment_id: &str,
        signal: ReloadSignal,
    ) -> Result<usize, RuntimeError> {
        self.signals
            .lock()
            .unwrap()
            .push((deployment_id.to_string(), signal));
        Ok(1)
    }

    async fn execute_health_check(
        &self,
        _instance_id: &str,
        _health_check: &HealthCheck,
    ) -> (HealthCheckStatus, Option<String>) {
        self.health_check_result.clone()
    }

    async fn get_logs(
        &self,
        _deployment_id: &str,
        _tail: Option<&str>,
        _since: Option<i32>,
        _container: Option<&str>,
    ) -> Vec<Log> {
        Vec::new()
    }

    async fn stream_logs(
        &self,
        _deployment_id: &str,
        _tail: Option<&str>,
        _since: Option<i32>,
        _container: Option<&str>,
    ) -> Pin<Box<dyn futures::Stream<Item = Result<Event, Infallible>> + Send>> {
        Box::pin(futures::stream::empty())
    }

    async fn get_instance_stats(&self, _deployment_id: &str) -> Vec<InstanceStatsOutput> {
        self.instance_stats.clone()
    }
}
