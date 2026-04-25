use crate::models::config;
use crate::models::config::Config;
use crate::models::deployment_event;
use crate::models::deployments::{self, Deployment, DeploymentStatus, EnvValue};
use crate::models::health_check_logs;
use crate::models::secret as SecretModel;
use crate::runtime::lifecycle_trait::RuntimeLifecycle;
use crate::scheduler::docker_events::DockerEvent;
use crate::scheduler::health_checker::HealthChecker;
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::env;
use std::sync::Arc;
use tokio::sync::mpsc;
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
    match config::find_by_namespace(pool, &deployment.namespace).await {
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

async fn apply_runtime(
    pool: &SqlitePool,
    deployment: &Deployment,
    resolved: Deployment,
    resolved_mounts: Vec<crate::models::volume::ResolvedMount>,
    apply_timeout: Duration,
    apply_timeout_secs: u64,
    runtime: &dyn RuntimeLifecycle,
) -> Option<Deployment> {
    match tokio::time::timeout(apply_timeout, runtime.apply(resolved, resolved_mounts)).await {
        Ok(result) => Some(result),
        Err(_) => {
            error!("runtime apply timed out for deployment {}", deployment.id);
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
    runtime: &dyn RuntimeLifecycle,
) {
    if deployment.status != DeploymentStatus::Running || deployment.health_checks.is_empty() {
        return;
    }

    debug!("Executing health checks for deployment {}", deployment.id);

    let outcome = health_checker.execute_checks(deployment, runtime).await;

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
        runtime.remove_instance(instance_id.clone()).await;
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

/// Handle rolling update coordination for deployments that have a `parent_id`.
///
/// Called after `apply_runtime` + `run_health_checks` for each child deployment.
/// - If the child is `Running` (healthy): remove one instance from the parent.
///   When the parent reaches 0 instances, mark it as `Deleted` and clear `parent_id`.
/// - If the child is `Failed`: stop the rollout — parent containers keep running.
async fn handle_rolling_update(pool: &SqlitePool, child: &mut Deployment, deleted: &mut Vec<String>, runtime: &dyn RuntimeLifecycle) {
    let parent_id = match &child.parent_id {
        Some(id) => id.clone(),
        None => return,
    };

    // If child failed health checks, stop the rollout — leave parent alone.
    if child.status == DeploymentStatus::Failed || child.status == DeploymentStatus::Deleted {
        warn!(
            "Rolling update failed for deployment {} (parent: {}): child health checks failed",
            child.id, parent_id
        );
        if let Err(e) = deployment_event::log_event(
            pool,
            child.id.clone(),
            "error",
            format!("Rolling update failed: health checks did not pass. Parent deployment {} is still running.", parent_id),
            "scheduler",
            Some("RollingUpdateFailed"),
        )
        .await
        {
            warn!("Failed to log rolling update failure event: {}", e);
        }
        return;
    }

    // Only proceed when the child has at least one running instance.
    if child.status != DeploymentStatus::Running || child.instances.is_empty() {
        return;
    }

    // Load the parent deployment.
    let mut parent = match deployments::find(pool, &parent_id).await {
        Ok(Some(d)) => d,
        Ok(None) => {
            // Parent is already gone — just clear parent_id on the child.
            info!(
                "Rolling update: parent {} no longer exists, clearing parent_id on child {}",
                parent_id, child.id
            );
            child.parent_id = None;
            return;
        }
        Err(e) => {
            error!("Failed to load parent deployment {}: {}", parent_id, e);
            return;
        }
    };

    // Refresh parent's live instance list.
    parent.instances = runtime.list_instances(parent.id.clone(), "active").await;

    if parent.instances.is_empty() {
        // All parent instances are gone — finalize the rollout.
        info!(
            "Rolling update complete: parent {} has 0 instances, marking as deleted",
            parent.id
        );
        parent.status = DeploymentStatus::Deleted;
        if let Err(e) = deployments::update(pool, &parent).await {
            error!("Failed to mark parent {} as deleted: {}", parent.id, e);
        }
        // Parent will be cleaned up in the next cleanup_deleted pass — add it to the list.
        deleted.push(parent.id.clone());

        child.parent_id = None;

        if let Err(e) = deployment_event::log_event(
            pool,
            child.id.clone(),
            "info",
            format!("Rolling update complete: replaced parent deployment {}", parent_id),
            "scheduler",
            Some("RollingUpdateComplete"),
        )
        .await
        {
            warn!("Failed to log rolling update complete event: {}", e);
        }
    } else {
        // Remove one instance from the parent — one step per scheduler cycle.
        let instance_id = parent.instances[0].clone();
        if runtime.remove_instance(instance_id.clone()).await {
            parent.instances.remove(0);
            info!(
                "Rolling update: removed instance {} from parent {} ({} remaining)",
                instance_id,
                parent.id,
                parent.instances.len()
            );
        } else {
            warn!(
                "Rolling update: failed to remove instance {} from parent {}, will retry next cycle",
                instance_id, parent.id
            );
            return;
        }

        if let Err(e) = deployment_event::log_event(
            pool,
            child.id.clone(),
            "info",
            format!(
                "Rolling update: removed instance {} from parent {} ({} remaining)",
                instance_id,
                parent_id,
                parent.instances.len()
            ),
            "scheduler",
            Some("RollingUpdateStep"),
        )
        .await
        {
            warn!("Failed to log rolling update step event: {}", e);
        }
    }
}

/// Drain all Docker events currently in the channel and apply their effects to
/// the database. Non-blocking: returns as soon as the channel is empty so the
/// scheduler can proceed to its reconciliation pass.
///
/// On every event that signals an instance has died (die / oom / kill), bump
/// `restart_count` for the deployment and log a deployment_event so the user
/// can see the crash trace. Once `restart_count` reaches `MAX_RESTART_COUNT`,
/// the existing logic in `lifecycle::handle_worker_deployment` flips the
/// status to `CrashLoopBackOff` and stops respawning — that's what bounds the
/// loop and prevents disk saturation.
async fn drain_docker_events(pool: &SqlitePool, event_rx: &mut mpsc::Receiver<DockerEvent>) {
    loop {
        match event_rx.try_recv() {
            Ok(event) => apply_docker_event(pool, event).await,
            Err(mpsc::error::TryRecvError::Empty) => return,
            Err(mpsc::error::TryRecvError::Disconnected) => {
                error!("Docker event channel disconnected — listener task likely died");
                return;
            }
        }
    }
}

async fn apply_docker_event(pool: &SqlitePool, event: DockerEvent) {
    match event {
        DockerEvent::ContainerDied { deployment_id, container_id, exit_code } => {
            bump_restart_count(
                pool,
                &deployment_id,
                format!(
                    "Container {} died (exit_code={})",
                    container_id,
                    exit_code.map(|c| c.to_string()).unwrap_or_else(|| "?".to_string()),
                ),
                "ContainerDied",
            )
            .await;
        }
        DockerEvent::ContainerOom { deployment_id, container_id } => {
            // Docker emits `oom` then `die`; we count on `die` so we don't double-count.
            // This branch only logs the OOM cause for traceability.
            if let Err(e) = deployment_event::log_event(
                pool,
                deployment_id,
                "warn",
                format!("Container {} killed by OOM", container_id),
                "docker-events",
                Some("ContainerOom"),
            )
            .await
            {
                warn!("Failed to log OOM event: {}", e);
            }
        }
        DockerEvent::ContainerKilled { deployment_id, container_id, signal } => {
            // A `kill` event is fired for any signal sent to the container,
            // including the SIGTERM Ring itself sends on scale-down or delete.
            // We only log it for traceability and let `die` carry the count.
            if let Err(e) = deployment_event::log_event(
                pool,
                deployment_id,
                "info",
                format!(
                    "Container {} received signal {}",
                    container_id,
                    signal.unwrap_or_else(|| "?".to_string()),
                ),
                "docker-events",
                Some("ContainerKilled"),
            )
            .await
            {
                warn!("Failed to log kill event: {}", e);
            }
        }
        DockerEvent::ContainerStarted { .. } => {
            // No-op for now: the scheduler already detects healthy containers
            // via `list_instances` on the next reconciliation pass.
        }
    }
}

async fn bump_restart_count(pool: &SqlitePool, deployment_id: &str, message: String, reason: &str) {
    let mut deployment = match deployments::find(pool, deployment_id).await {
        Ok(Some(d)) => d,
        Ok(None) => {
            // Container belonged to a deployment that has since been deleted — ignore.
            debug!("Ignoring event for unknown deployment {}", deployment_id);
            return;
        }
        Err(e) => {
            error!("Failed to load deployment {} on event: {}", deployment_id, e);
            return;
        }
    };

    // Don't keep counting once we've already given up — saves DB writes when a
    // doomed deployment keeps emitting events.
    if deployment.status == DeploymentStatus::CrashLoopBackOff {
        return;
    }

    deployment.restart_count += 1;
    if let Err(e) = deployments::update(pool, &deployment).await {
        error!(
            "Failed to persist restart_count for deployment {}: {}",
            deployment_id, e
        );
        return;
    }

    if let Err(e) = deployment_event::log_event(
        pool,
        deployment_id.to_string(),
        "warn",
        format!("{} — restart_count={}", message, deployment.restart_count),
        "docker-events",
        Some(reason),
    )
    .await
    {
        warn!("Failed to log crash event for {}: {}", deployment_id, e);
    }
}

pub(crate) async fn schedule(
    pool: SqlitePool,
    config: crate::config::config::Config,
    runtimes: std::sync::Arc<HashMap<String, Arc<dyn RuntimeLifecycle>>>,
    mut event_rx: mpsc::Receiver<DockerEvent>,
) {
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
        // Apply any crash events received from Docker since the last cycle.
        // Doing this before `find_all` ensures that the deployments we load
        // already reflect the latest restart_count, so the worker scaler can
        // hit CrashLoopBackOff in the same cycle as the crash that caused it.
        drain_docker_events(&pool, &mut event_rx).await;

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
            let runtime = match runtimes.get(&deployment.runtime) {
                Some(rt) => rt.clone(),
                None => {
                    debug!("No runtime registered for '{}', skipping deployment {}", deployment.runtime, deployment.id);
                    continue;
                }
            };

            let configs = match load_configs(&pool, &deployment).await {
                Some(c) => c,
                None => continue,
            };

            let resolved = match prepare_deployment(&pool, &deployment).await {
                Some(d) => d,
                None => continue,
            };

            let resolved_mounts =
                match crate::models::volume::resolve_volumes(&deployment.volumes, &configs) {
                    Ok(mounts) => mounts,
                    Err(e) => {
                        error!(
                            "Failed to resolve volumes for deployment {}: {}",
                            deployment.id, e
                        );
                        continue;
                    }
                };

            let mut result = match apply_runtime(
                &pool,
                &deployment,
                resolved,
                resolved_mounts,
                apply_timeout,
                apply_timeout_secs,
                runtime.as_ref(),
            )
            .await
            {
                Some(d) => d,
                None => continue,
            };

            persist_pending_events(&pool, &mut result).await;

            // Re-read current status from DB to detect concurrent changes (e.g. API delete)
            if let Ok(Some(current)) = deployments::find(&pool, &result.id).await {
                if current.status == DeploymentStatus::Deleted && result.status != DeploymentStatus::Deleted {
                    info!(
                        "Deployment {} was deleted externally during scheduler cycle, skipping update",
                        result.id
                    );
                    continue;
                }
            }

            handle_status_transitions(&pool, &mut result, &mut deleted).await;
            run_health_checks(&pool, &mut result, &health_checker, runtime.as_ref()).await;
            handle_rolling_update(&pool, &mut result, &mut deleted, runtime.as_ref()).await;

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
