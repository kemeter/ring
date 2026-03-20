use crate::models::config;
use crate::models::config::Config;
use crate::models::deployment_event;
use crate::models::deployments::{self, Deployment, DeploymentStatus, EnvValue};
use crate::models::health_check_logs;
use crate::models::secret as SecretModel;
use crate::runtime::docker;
use crate::runtime::runtime::Runtime;
use crate::scheduler::health_checker::HealthChecker;
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::env;
use tokio::time::{Duration, Instant, sleep};

async fn resolve_environment(deployment: &mut Deployment, pool: &SqlitePool) -> Result<(), String> {
    let mut resolved = HashMap::new();

    for (key, env_value) in deployment.environment.iter() {
        let value = match env_value {
            EnvValue::Plain(v) => EnvValue::Plain(v.clone()),
            EnvValue::SecretRef { secret_ref } => {
                match SecretModel::find_by_namespace_name(pool, &deployment.namespace, secret_ref)
                    .await
                {
                    Ok(Some(secret)) => match secret.get_decrypted_value() {
                        Ok(v) => EnvValue::Plain(v),
                        Err(e) => {
                            return Err(format!(
                                "Failed to decrypt secret '{}': {}",
                                secret_ref, e
                            ));
                        }
                    },
                    Ok(None) => {
                        return Err(format!(
                            "Secret '{}' not found in namespace '{}'",
                            secret_ref, deployment.namespace
                        ));
                    }
                    Err(e) => {
                        return Err(format!("Failed to fetch secret '{}': {}", secret_ref, e));
                    }
                }
            }
        };
        resolved.insert(key.clone(), value);
    }

    deployment.environment = resolved;
    Ok(())
}

async fn load_configs(
    pool: &SqlitePool,
    deployment: &Deployment,
) -> Option<HashMap<String, Config>> {
    match config::find_by_namespace(pool, deployment.namespace.clone()).await {
        Ok(configs_vec) => Some(
            configs_vec
                .into_iter()
                .map(|c| (c.name.clone(), c))
                .collect(),
        ),
        Err(e) => {
            error!(
                "Failed to load configs for deployment {}: {}",
                deployment.id, e
            );
            if let Err(e) = deployment_event::log_event(
                pool,
                deployment.id.clone(),
                "error",
                format!("Failed to load configs: {}", e),
                "scheduler",
                Some("ConfigLoadError"),
            )
            .await
            {
                warn!("Failed to log config load error event: {}", e);
            }
            None
        }
    }
}

async fn prepare_deployment(pool: &SqlitePool, deployment: &Deployment) -> Option<Deployment> {
    let mut resolved = deployment.clone();
    if let Err(e) = resolve_environment(&mut resolved, pool).await {
        error!(
            "Failed to resolve secrets for deployment {}: {}",
            deployment.id, e
        );
        if let Err(log_err) = deployment_event::log_event(
            pool,
            deployment.id.clone(),
            "error",
            format!("Failed to resolve secrets: {}", e),
            "scheduler",
            Some("SecretResolutionError"),
        )
        .await
        {
            warn!("Failed to log secret resolution error event: {}", log_err);
        }
        return None;
    }
    Some(resolved)
}

async fn apply_docker(
    pool: &SqlitePool,
    deployment: &Deployment,
    resolved: Deployment,
    configs: HashMap<String, Config>,
    apply_timeout: Duration,
    apply_timeout_secs: u64,
) -> Option<Deployment> {
    match tokio::time::timeout(apply_timeout, docker::apply(resolved, configs)).await {
        Ok(result) => Some(result),
        Err(_) => {
            error!("docker::apply timed out for deployment {}", deployment.id);
            if let Err(e) = deployment_event::log_event(
                pool,
                deployment.id.clone(),
                "error",
                format!(
                    "Scheduler apply timed out after {} seconds",
                    apply_timeout_secs
                ),
                "scheduler",
                Some("ApplyTimeout"),
            )
            .await
            {
                warn!("Failed to log apply timeout event: {}", e);
            }
            None
        }
    }
}

async fn persist_pending_events(pool: &SqlitePool, deployment: &mut Deployment) {
    for event in &deployment.pending_events {
        if let Err(e) = deployment_event::create_event(pool, event).await {
            warn!(
                "Failed to persist runtime event for deployment {}: {}",
                deployment.id, e
            );
        }
    }
    deployment.pending_events.clear();
}

async fn handle_status_transitions(
    pool: &SqlitePool,
    deployment: &mut Deployment,
    deleted: &mut Vec<String>,
) {
    if deployment.status == DeploymentStatus::Deleted && deployment.instances.is_empty() {
        info!("Marking deployment {} for cleanup", deployment.id);
        if let Err(e) = deployment_event::log_event(
            pool,
            deployment.id.clone(),
            "info",
            "Deployment marked for cleanup - all containers stopped".to_string(),
            "scheduler",
            Some("CleanupScheduled"),
        )
        .await
        {
            warn!(
                "Failed to log cleanup event for deployment {}: {}",
                deployment.id, e
            );
        }
        deleted.push(deployment.id.clone());
    }

    if deployment.status == DeploymentStatus::Creating && !deployment.instances.is_empty() {
        info!(
            "Deployment {} transition: creating -> running",
            deployment.id
        );
        if let Err(e) = deployment_event::log_event(
            pool,
            deployment.id.clone(),
            "info",
            format!(
                "Status changed from creating to running ({} containers)",
                deployment.instances.len()
            ),
            "scheduler",
            Some("StateTransition"),
        )
        .await
        {
            warn!(
                "Failed to log state transition event for deployment {}: {}",
                deployment.id, e
            );
        }
        deployment.status = DeploymentStatus::Running;
    }
}

async fn run_health_checks(
    pool: &SqlitePool,
    deployment: &mut Deployment,
    health_checker: &HealthChecker,
) {
    if deployment.status != DeploymentStatus::Running || deployment.health_checks.is_empty() {
        return;
    }

    debug!("Executing health checks for deployment {}", deployment.id);
    let rt = match Runtime::new(deployment.clone()) {
        Ok(rt) => rt,
        Err(_) => return,
    };

    let outcome = health_checker.execute_checks(deployment, rt.as_ref()).await;

    for result in &outcome.results {
        health_checker.store_result(result).await;
    }

    for event in &outcome.events {
        if let Err(e) = deployment_event::create_event(pool, event).await {
            warn!(
                "Failed to persist health check event for deployment {}: {}",
                deployment.id, e
            );
        }
    }

    if let Some(new_status) = outcome.proposed_status {
        deployment.status = new_status;
    }

    for instance_id in &outcome.instances_to_remove {
        rt.remove_instance(instance_id).await;
        deployment.instances.retain(|id| id != instance_id);
    }
}

async fn cleanup_deleted(pool: &SqlitePool, deleted: Vec<String>) {
    if deleted.is_empty() {
        return;
    }

    info!("Cleaning up {} deployments", deleted.len());

    for id in &deleted {
        if let Ok(count) = deployment_event::delete_by_deployment_id(pool, id).await {
            debug!("Deleted {} events for deployment {}", count, id);
        }
        if let Ok(count) = health_check_logs::delete_by_deployment_id(pool, id).await {
            debug!("Deleted {} health checks for deployment {}", count, id);
        }
    }

    if let Err(e) = deployments::delete_batch(pool, deleted).await {
        error!("Failed to delete deployments: {}", e);
    }
}

pub(crate) async fn schedule(pool: SqlitePool, config: crate::config::config::Config) {
    let interval_seconds = env::var("RING_SCHEDULER_INTERVAL")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(config.scheduler.interval);

    let apply_timeout_secs = env::var("RING_APPLY_TIMEOUT")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(300);
    let apply_timeout = Duration::from_secs(apply_timeout_secs);

    let duration = Duration::from_secs(interval_seconds);
    let health_checker = HealthChecker::new(pool.clone());

    let cleanup_interval = Duration::from_secs(300);
    let mut last_cleanup = Instant::now();

    info!(
        "Starting scheduler with interval: {}s, apply timeout: {}s",
        interval_seconds, apply_timeout_secs
    );

    loop {
        let mut filters = HashMap::new();
        filters.insert(
            String::from("status"),
            vec![
                String::from("creating"),
                String::from("running"),
                String::from("deleted"),
            ],
        );
        let list_deployments = match deployments::find_all(&pool, filters).await {
            Ok(list) => list,
            Err(e) => {
                error!("Failed to fetch deployments: {}", e);
                sleep(duration).await;
                continue;
            }
        };

        info!("Processing {} deployments", list_deployments.len());
        let mut deleted: Vec<String> = Vec::new();

        for deployment in list_deployments.into_iter() {
            if deployment.runtime != "docker" {
                continue;
            }

            let configs = match load_configs(&pool, &deployment).await {
                Some(c) => c,
                None => continue,
            };

            let resolved = match prepare_deployment(&pool, &deployment).await {
                Some(d) => d,
                None => continue,
            };

            let mut result = match apply_docker(
                &pool,
                &deployment,
                resolved,
                configs,
                apply_timeout,
                apply_timeout_secs,
            )
            .await
            {
                Some(d) => d,
                None => continue,
            };

            persist_pending_events(&pool, &mut result).await;
            handle_status_transitions(&pool, &mut result, &mut deleted).await;
            run_health_checks(&pool, &mut result, &health_checker).await;

            if let Err(e) = deployments::update(&pool, &result).await {
                error!("Failed to update deployment {}: {}", result.id, e);
            }
        }

        cleanup_deleted(&pool, deleted).await;

        if last_cleanup.elapsed() >= cleanup_interval {
            last_cleanup = Instant::now();
            if let Err(e) = health_check_logs::cleanup_old_health_checks(&pool).await {
                error!("Failed to cleanup old health checks: {}", e);
            }
        }

        debug!(
            "Scheduler cycle completed, sleeping for {}s",
            duration.as_secs()
        );
        sleep(duration).await;
    }
}
