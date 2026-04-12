use crate::api::dto::stats::InstanceStatsOutput;
use crate::models::deployments::Deployment;
use crate::models::health_check::{HealthCheck, HealthCheckStatus};
use crate::models::volume::ResolvedMount;
use crate::runtime::lifecycle_trait::{Log, RuntimeLifecycle};
use async_trait::async_trait;
use axum::response::sse::Event;
use std::convert::Infallible;
use std::pin::Pin;

pub(crate) struct MockRuntime {
    health_check_result: (HealthCheckStatus, Option<String>),
}

impl MockRuntime {
    pub(crate) fn healthy() -> Self {
        Self {
            health_check_result: (HealthCheckStatus::Success, None),
        }
    }

    pub(crate) fn unhealthy(message: &str) -> Self {
        Self {
            health_check_result: (
                HealthCheckStatus::Failed,
                Some(message.to_string()),
            ),
        }
    }
}

#[async_trait]
impl RuntimeLifecycle for MockRuntime {
    async fn apply(&self, deployment: Deployment, _resolved_mounts: Vec<ResolvedMount>) -> Deployment {
        deployment
    }

    async fn list_instances(&self, _deployment_id: String, _status: &str) -> Vec<String> {
        Vec::new()
    }

    async fn remove_instance(&self, _instance_id: String) -> bool {
        true
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
        Vec::new()
    }
}
