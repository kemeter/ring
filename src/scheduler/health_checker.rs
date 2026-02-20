use crate::models::health_check::{HealthCheck, HealthCheckResult, HealthCheckStatus, FailureAction};
use crate::models::deployments::{Deployment, DeploymentStatus, self as deployments};
use crate::models::deployment_event;
use sqlx::SqlitePool;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::time::timeout;
use std::collections::HashMap;
use uuid::Uuid;
use chrono::Utc;

pub(crate) struct HealthChecker {
    pool: SqlitePool,
    // Track failure counts per deployment+health_check combination
    failure_counts: Arc<Mutex<HashMap<String, u32>>>,
}

impl HealthChecker {
    pub(crate) fn new(pool: SqlitePool) -> Self {
        Self {
            pool,
            failure_counts: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub(crate) async fn execute_checks(&self, deployment: &Deployment) -> Vec<HealthCheckResult> {
        let mut results = Vec::new();

        if deployment.status != DeploymentStatus::Running {
            return results;
        }

        let runtime = match crate::runtime::runtime::Runtime::new(deployment.clone()) {
            Ok(r) => r,
            Err(e) => {
                log::error!("Failed to connect to runtime for deployment {}: {}", deployment.name, e);
                return results;
            }
        };
        for instance_id in &deployment.instances {
            for (hc_index, health_check) in deployment.health_checks.iter().enumerate() {
                let result = self.execute_single_check_with_runtime(&runtime, deployment, health_check, instance_id).await;

                self.store_result(&result).await;
                let key = format!("{}:{}:{}", deployment.id, instance_id, hc_index);
                if matches!(result.status, HealthCheckStatus::Failed | HealthCheckStatus::Timeout) {
                    let should_trigger_action = self.increment_failure_count(&key, health_check.threshold()).await;
                    if should_trigger_action {
                        self.handle_failure_with_runtime(&runtime, deployment, health_check, &result, instance_id).await;
                        self.reset_failure_count(&key).await;
                    }
                } else {
                    self.reset_failure_count(&key).await;
                }

                results.push(result);
            }
        }

        results
    }

    async fn execute_single_check_with_runtime(&self, runtime: &Box<dyn crate::runtime::runtime::RuntimeInterface + Send + Sync>, deployment: &Deployment, health_check: &HealthCheck, instance_id: &str) -> HealthCheckResult {
        let created_time = Utc::now();
        let start_time = Utc::now();
        let timeout_duration = match HealthCheck::parse_duration(health_check.timeout()) {
            Ok(duration) => duration,
            Err(e) => {
                return HealthCheckResult {
                    id: Uuid::new_v4().to_string(),
                    deployment_id: deployment.id.clone(),
                    check_type: health_check.check_type().to_string(),
                    status: HealthCheckStatus::Failed,
                    message: Some(format!("Invalid timeout duration: {}", e)),
                    created_at: created_time.to_rfc3339(),
                    started_at: start_time.to_rfc3339(),
                    finished_at: Utc::now().to_rfc3339(),
                };
            }
        };

        let result = timeout(timeout_duration, async {
            runtime.execute_health_check(instance_id, health_check).await
        }).await;

        let end_time = Utc::now();

        match result {
            Ok(check_result) => HealthCheckResult {
                id: Uuid::new_v4().to_string(),
                deployment_id: deployment.id.clone(),
                check_type: health_check.check_type().to_string(),
                status: check_result.0,
                message: check_result.1,
                created_at: created_time.to_rfc3339(),
                started_at: start_time.to_rfc3339(),
                finished_at: end_time.to_rfc3339(),
            },
            Err(_) => HealthCheckResult {
                id: Uuid::new_v4().to_string(),
                deployment_id: deployment.id.clone(),
                check_type: health_check.check_type().to_string(),
                status: HealthCheckStatus::Timeout,
                message: Some("Health check timed out".to_string()),
                created_at: created_time.to_rfc3339(),
                started_at: start_time.to_rfc3339(),
                finished_at: end_time.to_rfc3339(),
            }
        }
    }


    async fn store_result(&self, result: &HealthCheckResult) {
        debug!("Attempting to store health check result for deployment: {}", result.deployment_id);

        let status_str = match result.status {
            HealthCheckStatus::Success => "success",
            HealthCheckStatus::Failed => "failed",
            HealthCheckStatus::Timeout => "timeout",
        };

        let message = result.message.as_deref();
        if let Err(e) = sqlx::query(
            "INSERT INTO health_check (id, deployment_id, check_type, status, message, created_at, started_at, finished_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)"
        )
            .bind(&result.id)
            .bind(&result.deployment_id)
            .bind(&result.check_type)
            .bind(status_str)
            .bind(message)
            .bind(&result.created_at)
            .bind(&result.started_at)
            .bind(&result.finished_at)
            .execute(&self.pool)
            .await
        {
            error!("Failed to store health check result for deployment {}: {}", result.deployment_id, e);
        } else {
            debug!("Health check result stored for deployment {}: {:?}", result.deployment_id, result.status);
        }
    }


    async fn increment_failure_count(&self, key: &str, threshold: u32) -> bool {
        let mut counts = self.failure_counts.lock().await;
        let current_count = counts.entry(key.to_string()).or_insert(0);
        *current_count += 1;

        debug!("Health check failure count for {}: {}/{}", key, *current_count, threshold);

        *current_count >= threshold
    }

    async fn reset_failure_count(&self, key: &str) {
        let mut counts = self.failure_counts.lock().await;
        if counts.remove(key).is_some() {
            debug!("Reset failure count for {}", key);
        }
    }

    async fn handle_failure_with_runtime(&self, runtime: &Box<dyn crate::runtime::runtime::RuntimeInterface + Send + Sync>, deployment: &Deployment, health_check: &HealthCheck, result: &HealthCheckResult, instance_id: &str) {
        let action = health_check.on_failure();

        match action {
            FailureAction::Restart => {
                info!("Health check failed for instance {} in deployment {}, triggering restart", instance_id, deployment.id);

                // Log event
                let _ = deployment_event::log_event(
                    &self.pool,
                    deployment.id.clone(),
                    "warning",
                    format!("Health check failed for instance {} ({}), triggering instance restart", instance_id, result.message.as_deref().unwrap_or("unknown error")),
                    "health_checker",
                    Some("HealthCheckInstanceRestart")
                ).await;

                // Remove the failing instance using runtime - scheduler will recreate it automatically
                runtime.remove_instance(instance_id).await;

                // Update deployment instance list in database
                if let Ok(Some(mut updated_deployment)) = deployments::find(&self.pool, deployment.id.clone()).await {
                    updated_deployment.instances.retain(|id| id != instance_id);
                    if let Err(e) = deployments::update(&self.pool, &updated_deployment).await {
                        error!("Failed to update deployment {}: {}", updated_deployment.id, e);
                    }
                    info!("Updated deployment {} instances list (removed {})", updated_deployment.id, instance_id);
                }
            },
            FailureAction::Stop => {
                info!("Health check failed for instance {} in deployment {}, triggering deployment stop", instance_id, deployment.id);

                // Stop deployment by changing status to deleted (actually stops containers)
                let mut updated_deployment = deployment.clone();
                updated_deployment.status = DeploymentStatus::Deleted;

                if let Err(e) = deployments::update(&self.pool, &updated_deployment).await {
                    error!("Failed to update deployment {}: {}", updated_deployment.id, e);
                }
                let _ = deployment_event::log_event(
                    &self.pool,
                    deployment.id.clone(),
                    "warning",
                    format!("Health check failed for instance {} ({}), triggering deployment stop", instance_id, result.message.as_deref().unwrap_or("unknown error")),
                    "health_checker",
                    Some("HealthCheckStop")
                ).await;
                info!("Deployment {} status changed to deleted by health checker due to instance {} failure", deployment.id, instance_id);
            },
            FailureAction::Alert => {
                info!("Health check failed for instance {} in deployment {}, sending alert", instance_id, deployment.id);

                let _ = deployment_event::log_event(
                    &self.pool,
                    deployment.id.clone(),
                    "error",
                    format!("Health check failed for instance {}: {}", instance_id, result.message.as_deref().unwrap_or("unknown error")),
                    "health_checker",
                    Some("HealthCheckAlert")
                ).await;
            },
        }
    }
}
