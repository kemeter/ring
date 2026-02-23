use crate::models::health_check::{HealthCheck, HealthCheckResult, HealthCheckStatus, FailureAction};
use crate::models::deployments::{Deployment, DeploymentStatus};
use crate::models::deployment_event::DeploymentEvent;
use sqlx::SqlitePool;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::time::timeout;
use std::collections::HashMap;
use uuid::Uuid;
use chrono::Utc;

pub(crate) struct HealthCheckOutcome {
    pub(crate) results: Vec<HealthCheckResult>,
    pub(crate) events: Vec<DeploymentEvent>,
    pub(crate) proposed_status: Option<DeploymentStatus>,
    pub(crate) instances_to_remove: Vec<String>,
}

pub(crate) struct HealthChecker {
    pool: SqlitePool,
    failure_counts: Arc<Mutex<HashMap<String, u32>>>,
}

impl HealthChecker {
    pub(crate) fn new(pool: SqlitePool) -> Self {
        Self {
            pool,
            failure_counts: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub(crate) async fn execute_checks(
        &self,
        deployment: &Deployment,
        runtime: &(dyn crate::runtime::runtime::RuntimeInterface + Send + Sync),
    ) -> HealthCheckOutcome {
        let mut outcome = HealthCheckOutcome {
            results: Vec::new(),
            events: Vec::new(),
            proposed_status: None,
            instances_to_remove: Vec::new(),
        };

        if deployment.status != DeploymentStatus::Running {
            return outcome;
        }

        for instance_id in &deployment.instances {
            for (hc_index, health_check) in deployment.health_checks.iter().enumerate() {
                let result = self.execute_single_check_with_runtime(runtime, deployment, health_check, instance_id).await;

                let key = format!("{}:{}:{}", deployment.id, instance_id, hc_index);
                if matches!(result.status, HealthCheckStatus::Failed | HealthCheckStatus::Timeout) {
                    let should_trigger_action = self.increment_failure_count(&key, health_check.threshold()).await;
                    if should_trigger_action {
                        self.handle_failure(&mut outcome, deployment, health_check, &result, instance_id);
                        self.reset_failure_count(&key).await;
                    }
                } else {
                    self.reset_failure_count(&key).await;
                }

                outcome.results.push(result);
            }
        }

        outcome
    }

    async fn execute_single_check_with_runtime(&self, runtime: &(dyn crate::runtime::runtime::RuntimeInterface + Send + Sync), deployment: &Deployment, health_check: &HealthCheck, instance_id: &str) -> HealthCheckResult {
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

    pub(crate) async fn store_result(&self, result: &HealthCheckResult) {
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

    fn handle_failure(&self, outcome: &mut HealthCheckOutcome, deployment: &Deployment, health_check: &HealthCheck, result: &HealthCheckResult, instance_id: &str) {
        let action = health_check.on_failure();

        match action {
            FailureAction::Restart => {
                info!("Health check failed for instance {} in deployment {}, triggering restart", instance_id, deployment.id);

                outcome.events.push(DeploymentEvent::new(
                    deployment.id.clone(),
                    "warning",
                    format!("Health check failed for instance {} ({}), triggering instance restart", instance_id, result.message.as_deref().unwrap_or("unknown error")),
                    "health_checker",
                    Some("HealthCheckInstanceRestart"),
                ));

                outcome.instances_to_remove.push(instance_id.to_string());
            },
            FailureAction::Stop => {
                info!("Health check failed for instance {} in deployment {}, triggering deployment stop", instance_id, deployment.id);

                outcome.proposed_status = Some(DeploymentStatus::Deleted);
                outcome.events.push(DeploymentEvent::new(
                    deployment.id.clone(),
                    "warning",
                    format!("Health check failed for instance {} ({}), triggering deployment stop", instance_id, result.message.as_deref().unwrap_or("unknown error")),
                    "health_checker",
                    Some("HealthCheckStop"),
                ));

                info!("Deployment {} status changed to deleted by health checker due to instance {} failure", deployment.id, instance_id);
            },
            FailureAction::Alert => {
                info!("Health check failed for instance {} in deployment {}, sending alert", instance_id, deployment.id);

                outcome.events.push(DeploymentEvent::new(
                    deployment.id.clone(),
                    "error",
                    format!("Health check failed for instance {}: {}", instance_id, result.message.as_deref().unwrap_or("unknown error")),
                    "health_checker",
                    Some("HealthCheckAlert"),
                ));
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bollard::Docker;
    use bollard::models::ContainerCreateBody;
    use bollard::query_parameters::{
        CreateContainerOptionsBuilder,
        StartContainerOptionsBuilder,
        StopContainerOptionsBuilder,
        RemoveContainerOptionsBuilder,
    };
    use sqlx::sqlite::SqlitePoolOptions;

    async fn new_test_pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("Could not create test database pool");

        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .expect("Could not execute database migrations");

        pool
    }

    async fn create_test_container(docker: &Docker, image: &str) -> String {
        let config = ContainerCreateBody {
            image: Some(image.to_string()),
            ..Default::default()
        };

        let options = CreateContainerOptionsBuilder::new().build();
        let response = docker
            .create_container(Some(options), config)
            .await
            .expect("Failed to create test container");

        let container_id = response.id;

        let start_options = StartContainerOptionsBuilder::new().build();
        docker
            .start_container(&container_id, Some(start_options))
            .await
            .expect("Failed to start test container");

        // Wait for the container to be fully started
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;

        container_id
    }

    async fn cleanup_container(docker: &Docker, container_id: &str) {
        let stop_options = StopContainerOptionsBuilder::new().build();
        let _ = docker.stop_container(container_id, Some(stop_options)).await;

        let remove_options = RemoveContainerOptionsBuilder::new().force(true).build();
        let _ = docker.remove_container(container_id, Some(remove_options)).await;
    }

    fn make_deployment(id: &str, instances: Vec<String>, health_checks: Vec<HealthCheck>) -> Deployment {
        Deployment {
            id: id.to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
            updated_at: None,
            status: DeploymentStatus::Running,
            restart_count: 0,
            namespace: "test".to_string(),
            name: "test-deployment".to_string(),
            image: "nginx:alpine".to_string(),
            config: None,
            runtime: "docker".to_string(),
            kind: "deployment".to_string(),
            replicas: 1,
            command: vec![],
            instances,
            labels: HashMap::new(),
            secrets: HashMap::new(),
            volumes: "".to_string(),
            health_checks,
            resources: None,
            image_digest: None,
            pending_events: vec![],
        }
    }

    #[tokio::test]
    async fn tcp_health_check_success() {
        let docker = Docker::connect_with_local_defaults().expect("Failed to connect to Docker");
        let container_id = create_test_container(&docker, "nginx:alpine").await;

        let pool = new_test_pool().await;
        let checker = HealthChecker::new(pool);

        let deployment = make_deployment(
            "test-tcp-success",
            vec![container_id.clone()],
            vec![HealthCheck::Tcp {
                port: 80,
                interval: "5s".to_string(),
                timeout: "10s".to_string(),
                threshold: 3,
                on_failure: FailureAction::Restart,
            }],
        );

        let runtime = crate::runtime::runtime::Runtime::new(deployment.clone()).expect("Failed to create runtime");
        let outcome = checker.execute_checks(&deployment, runtime.as_ref()).await;

        assert_eq!(outcome.results.len(), 1);
        assert!(matches!(outcome.results[0].status, HealthCheckStatus::Success));
        assert!(outcome.instances_to_remove.is_empty());
        assert!(outcome.proposed_status.is_none());

        cleanup_container(&docker, &container_id).await;
    }

    #[tokio::test]
    async fn tcp_health_check_failure_triggers_restart() {
        let docker = Docker::connect_with_local_defaults().expect("Failed to connect to Docker");
        let container_id = create_test_container(&docker, "nginx:alpine").await;

        let pool = new_test_pool().await;
        let checker = HealthChecker::new(pool);

        // TCP check on port 9999 which nginx doesn't listen on
        let deployment = make_deployment(
            "test-tcp-fail",
            vec![container_id.clone()],
            vec![HealthCheck::Tcp {
                port: 9999,
                interval: "5s".to_string(),
                timeout: "5s".to_string(),
                threshold: 1,
                on_failure: FailureAction::Restart,
            }],
        );

        let runtime = crate::runtime::runtime::Runtime::new(deployment.clone()).expect("Failed to create runtime");
        let outcome = checker.execute_checks(&deployment, runtime.as_ref()).await;

        assert_eq!(outcome.results.len(), 1);
        assert!(matches!(outcome.results[0].status, HealthCheckStatus::Failed));
        assert_eq!(outcome.instances_to_remove, vec![container_id.clone()]);
        assert!(outcome.events.iter().any(|e| e.reason.as_deref() == Some("HealthCheckInstanceRestart")));

        cleanup_container(&docker, &container_id).await;
    }

    #[tokio::test]
    async fn http_health_check_success() {
        let docker = Docker::connect_with_local_defaults().expect("Failed to connect to Docker");
        let container_id = create_test_container(&docker, "nginx:alpine").await;

        let pool = new_test_pool().await;
        let checker = HealthChecker::new(pool);

        let deployment = make_deployment(
            "test-http-success",
            vec![container_id.clone()],
            vec![HealthCheck::Http {
                url: "http://localhost:80/".to_string(),
                interval: "5s".to_string(),
                timeout: "10s".to_string(),
                threshold: 3,
                on_failure: FailureAction::Restart,
            }],
        );

        let runtime = crate::runtime::runtime::Runtime::new(deployment.clone()).expect("Failed to create runtime");
        let outcome = checker.execute_checks(&deployment, runtime.as_ref()).await;

        assert_eq!(outcome.results.len(), 1);
        assert!(matches!(outcome.results[0].status, HealthCheckStatus::Success));
        assert!(outcome.instances_to_remove.is_empty());

        cleanup_container(&docker, &container_id).await;
    }

    #[tokio::test]
    async fn command_health_check_success() {
        let docker = Docker::connect_with_local_defaults().expect("Failed to connect to Docker");
        let container_id = create_test_container(&docker, "nginx:alpine").await;

        let pool = new_test_pool().await;
        let checker = HealthChecker::new(pool);

        let deployment = make_deployment(
            "test-cmd-success",
            vec![container_id.clone()],
            vec![HealthCheck::Command {
                command: "true".to_string(),
                interval: "5s".to_string(),
                timeout: "10s".to_string(),
                threshold: 3,
                on_failure: FailureAction::Restart,
            }],
        );

        let runtime = crate::runtime::runtime::Runtime::new(deployment.clone()).expect("Failed to create runtime");
        let outcome = checker.execute_checks(&deployment, runtime.as_ref()).await;

        assert_eq!(outcome.results.len(), 1);
        assert!(matches!(outcome.results[0].status, HealthCheckStatus::Success));
        assert!(outcome.instances_to_remove.is_empty());

        cleanup_container(&docker, &container_id).await;
    }

    #[tokio::test]
    async fn threshold_counting() {
        let docker = Docker::connect_with_local_defaults().expect("Failed to connect to Docker");
        let container_id = create_test_container(&docker, "nginx:alpine").await;

        let pool = new_test_pool().await;
        let checker = HealthChecker::new(pool);

        // TCP on closed port with threshold=3
        let deployment = make_deployment(
            "test-threshold",
            vec![container_id.clone()],
            vec![HealthCheck::Tcp {
                port: 9999,
                interval: "5s".to_string(),
                timeout: "5s".to_string(),
                threshold: 3,
                on_failure: FailureAction::Restart,
            }],
        );

        let runtime = crate::runtime::runtime::Runtime::new(deployment.clone()).expect("Failed to create runtime");

        // Call 1: failure count = 1/3, no action
        let outcome1 = checker.execute_checks(&deployment, runtime.as_ref()).await;
        assert!(outcome1.instances_to_remove.is_empty());
        assert!(outcome1.events.is_empty());

        // Call 2: failure count = 2/3, no action
        let outcome2 = checker.execute_checks(&deployment, runtime.as_ref()).await;
        assert!(outcome2.instances_to_remove.is_empty());
        assert!(outcome2.events.is_empty());

        // Call 3: failure count = 3/3, triggers restart
        let outcome3 = checker.execute_checks(&deployment, runtime.as_ref()).await;
        assert_eq!(outcome3.instances_to_remove, vec![container_id.clone()]);
        assert!(outcome3.events.iter().any(|e| e.reason.as_deref() == Some("HealthCheckInstanceRestart")));

        cleanup_container(&docker, &container_id).await;
    }

    #[tokio::test]
    async fn failure_action_stop() {
        let docker = Docker::connect_with_local_defaults().expect("Failed to connect to Docker");
        let container_id = create_test_container(&docker, "nginx:alpine").await;

        let pool = new_test_pool().await;
        let checker = HealthChecker::new(pool);

        let deployment = make_deployment(
            "test-stop",
            vec![container_id.clone()],
            vec![HealthCheck::Tcp {
                port: 9999,
                interval: "5s".to_string(),
                timeout: "5s".to_string(),
                threshold: 1,
                on_failure: FailureAction::Stop,
            }],
        );

        let runtime = crate::runtime::runtime::Runtime::new(deployment.clone()).expect("Failed to create runtime");
        let outcome = checker.execute_checks(&deployment, runtime.as_ref()).await;

        assert_eq!(outcome.proposed_status, Some(DeploymentStatus::Deleted));
        assert!(outcome.events.iter().any(|e| e.reason.as_deref() == Some("HealthCheckStop")));
        assert!(outcome.instances_to_remove.is_empty());

        cleanup_container(&docker, &container_id).await;
    }

    #[tokio::test]
    async fn failure_action_alert() {
        let docker = Docker::connect_with_local_defaults().expect("Failed to connect to Docker");
        let container_id = create_test_container(&docker, "nginx:alpine").await;

        let pool = new_test_pool().await;
        let checker = HealthChecker::new(pool);

        let deployment = make_deployment(
            "test-alert",
            vec![container_id.clone()],
            vec![HealthCheck::Tcp {
                port: 9999,
                interval: "5s".to_string(),
                timeout: "5s".to_string(),
                threshold: 1,
                on_failure: FailureAction::Alert,
            }],
        );

        let runtime = crate::runtime::runtime::Runtime::new(deployment.clone()).expect("Failed to create runtime");
        let outcome = checker.execute_checks(&deployment, runtime.as_ref()).await;

        assert!(outcome.proposed_status.is_none());
        assert!(outcome.instances_to_remove.is_empty());
        assert!(outcome.events.iter().any(|e| e.reason.as_deref() == Some("HealthCheckAlert")));
        assert!(outcome.events.iter().any(|e| e.level == "error"));

        cleanup_container(&docker, &container_id).await;
    }

    #[tokio::test]
    async fn store_result_persists_to_db() {
        let pool = new_test_pool().await;
        let checker = HealthChecker::new(pool.clone());

        let result = HealthCheckResult {
            id: uuid::Uuid::new_v4().to_string(),
            deployment_id: "test-persist".to_string(),
            check_type: "tcp".to_string(),
            status: HealthCheckStatus::Success,
            message: Some("OK".to_string()),
            created_at: chrono::Utc::now().to_rfc3339(),
            started_at: chrono::Utc::now().to_rfc3339(),
            finished_at: chrono::Utc::now().to_rfc3339(),
        };

        checker.store_result(&result).await;

        let row = sqlx::query_as::<_, (String, String, String, String)>(
            "SELECT id, deployment_id, check_type, status FROM health_check WHERE id = ?"
        )
        .bind(&result.id)
        .fetch_one(&pool)
        .await
        .expect("Health check result should be persisted");

        assert_eq!(row.0, result.id);
        assert_eq!(row.1, "test-persist");
        assert_eq!(row.2, "tcp");
        assert_eq!(row.3, "success");
    }
}
