use crate::models::config;
use crate::models::config::Config;
use crate::models::deployment_event;
use crate::models::deployments::{self, Deployment, DeploymentStatus, EnvValue};
use crate::models::health_check_logs;
use crate::models::secret as SecretModel;
use crate::runtime::lifecycle_trait::RuntimeLifecycle;
use crate::scheduler::backoff::RetryBackoff;
use crate::scheduler::docker_events::DockerEvent;
use crate::scheduler::health_checker::HealthChecker;
use crate::scheduler::intentional_shutdowns::IntentionalShutdowns;
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
                Some("config_load_error"),
            )
            .await
            {
                warn!("Failed to log config load error event: {}", e);
            }
            None
        }
    }
}

/// Load only the secrets actually referenced as `type: secret` volumes on
/// this deployment. We avoid loading the full namespace's secret set so a
/// runaway deployment can't accidentally pull plaintext for unrelated
/// secrets into memory.
async fn load_secrets_for_volumes(
    pool: &SqlitePool,
    deployment: &Deployment,
) -> Option<HashMap<String, SecretModel::Secret>> {
    // The volumes field is JSON in the DB row; the same struct is later
    // re-parsed by resolve_volumes. Tolerate a parse error here by
    // returning an empty map — resolve_volumes will surface a clearer error.
    let volumes: Vec<crate::api::dto::deployment::DeploymentVolume> =
        serde_json::from_str(&deployment.volumes).unwrap_or_default();

    let mut secrets = HashMap::new();
    for volume in volumes.iter().filter(|v| v.r#type == "secret") {
        let name = match volume.source.as_ref() {
            Some(n) => n,
            None => continue, // resolve_volumes will report the missing source
        };

        if secrets.contains_key(name) {
            continue;
        }

        match SecretModel::find_by_namespace_name(pool, &deployment.namespace, name).await {
            Ok(Some(secret)) => {
                secrets.insert(name.clone(), secret);
            }
            Ok(None) => {
                // Don't fail here — let resolve_volumes produce the canonical
                // "Secret 'X' not found" error so the message format matches
                // what configs do.
            }
            Err(e) => {
                error!(
                    "Failed to load secret '{}' for deployment {}: {}",
                    name, deployment.id, e
                );
                if let Err(log_err) = deployment_event::log_event(
                    pool,
                    deployment.id.clone(),
                    "error",
                    format!("Failed to load secret '{}': {}", name, e),
                    "scheduler",
                    Some("secret_load_error"),
                )
                .await
                {
                    warn!("Failed to log secret load error event: {}", log_err);
                }
                return None;
            }
        }
    }
    Some(secrets)
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
            Some("secret_resolution_error"),
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
                Some("apply_timeout"),
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
            Some("cleanup_scheduled"),
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
            Some("state_transition"),
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

/// Built-in anti-flap window when the manifest doesn't override it. Mirrors
/// Nomad's `min_healthy_time` default. Override per readiness HC via the
/// `min_healthy_time` field on the check; the scheduler takes the maximum
/// across readiness checks so the most-cautious wins.
const DEFAULT_MIN_HEALTHY_TIME: Duration = Duration::from_secs(10);

/// Resolve the anti-flap window for a deployment: take the max of the
/// per-HC `min_healthy_time` (parsed via `HealthCheck::parse_duration`)
/// across readiness checks. Falls back to `DEFAULT_MIN_HEALTHY_TIME` when
/// nothing is set. Malformed values are logged at warn and ignored — the
/// rollout shouldn't stall just because someone typo'd a duration.
fn min_healthy_time_for(child: &Deployment) -> Duration {
    use crate::models::health_check::HealthCheck;

    let mut window = DEFAULT_MIN_HEALTHY_TIME;
    let mut overridden = false;

    for hc in child.health_checks.iter().filter(|hc| hc.is_readiness()) {
        if let Some(raw) = hc.min_healthy_time() {
            match HealthCheck::parse_duration(raw) {
                Ok(d) => {
                    if !overridden || d > window {
                        window = d;
                    }
                    overridden = true;
                }
                Err(e) => {
                    warn!(
                        "Invalid min_healthy_time '{}' on readiness HC for deployment {}: {} — using default",
                        raw, child.id, e
                    );
                }
            }
        }
    }

    window
}

/// True when the child either has no readiness HC at all (legacy behaviour)
/// or all of its readiness HCs have been green for the configured anti-flap
/// window.
async fn is_ready_to_drain(pool: &SqlitePool, child: &Deployment) -> bool {
    use crate::models::health_check::{ReadinessDecision, evaluate_readiness};

    let expected = child
        .health_checks
        .iter()
        .filter(|hc| hc.is_readiness())
        .count();

    if expected == 0 {
        // No readiness HC declared — legacy "drain on Running" semantics.
        return true;
    }

    // We need two things per check_type: (1) the latest status, to detect
    // Failing / PendingNoResult; (2) the **ready-since** timestamp, i.e. the
    // first success after the last non-success — anchoring the anti-flap
    // window to a stable point so the gate actually elapses instead of
    // being re-armed by every new success.
    let latest = match health_check_logs::find_latest_by_deployment(pool, child.id.clone()).await {
        Ok(rows) => rows,
        Err(e) => {
            warn!(
                "Failed to load readiness check results for deployment {}: {} — holding rollout",
                child.id, e
            );
            return false;
        }
    };
    let ready_since =
        match health_check_logs::find_ready_since_by_deployment(pool, child.id.clone()).await {
            Ok(rows) => rows,
            Err(e) => {
                warn!(
                    "Failed to load ready-since timestamps for deployment {}: {} — holding rollout",
                    child.id, e
                );
                return false;
            }
        };

    // Restrict to the check_types that are marked readiness in the manifest.
    let readiness_types: std::collections::HashSet<&str> = child
        .health_checks
        .iter()
        .filter(|hc| hc.is_readiness())
        .map(|hc| hc.check_type())
        .collect();

    let ready_since_by_type: std::collections::HashMap<&str, chrono::DateTime<chrono::Utc>> =
        ready_since
            .iter()
            .filter_map(|r| {
                chrono::DateTime::parse_from_rfc3339(&r.ready_since)
                    .ok()
                    .map(|t| (r.check_type.as_str(), t.with_timezone(&chrono::Utc)))
            })
            .collect();

    // We currently identify a HC by its `check_type`. That works as long as
    // a deployment doesn't declare two HCs of the same type (e.g. two
    // separate http probes) — the storage layer aggregates results by
    // check_type, so distinguishing two http checks would need a richer
    // identifier. Document the limitation; revisit if a real use case lands.
    let mut filtered: Vec<(
        crate::models::health_check::HealthCheckStatus,
        chrono::DateTime<chrono::Utc>,
    )> = Vec::new();
    for record in &latest {
        if !readiness_types.contains(record.check_type.as_str()) {
            continue;
        }
        let status = match record.status.as_str() {
            "success" => crate::models::health_check::HealthCheckStatus::Success,
            "timeout" => crate::models::health_check::HealthCheckStatus::Timeout,
            _ => crate::models::health_check::HealthCheckStatus::Failed,
        };
        // For success, anchor the timestamp to ready_since (stable). For
        // non-success, the timestamp is irrelevant — evaluate_readiness will
        // short-circuit to Failing.
        let anchor = if matches!(
            status,
            crate::models::health_check::HealthCheckStatus::Success
        ) {
            match ready_since_by_type.get(record.check_type.as_str()) {
                Some(t) => *t,
                None => continue,
            }
        } else {
            match chrono::DateTime::parse_from_rfc3339(&record.finished_at) {
                Ok(t) => t.with_timezone(&chrono::Utc),
                Err(_) => continue,
            }
        };
        filtered.push((status, anchor));
    }

    let min_healthy_time = min_healthy_time_for(child);
    let decision = evaluate_readiness(expected, &filtered, chrono::Utc::now(), min_healthy_time);

    match decision {
        ReadinessDecision::Ready => true,
        ReadinessDecision::NotConfigured => true, // expected == 0 case, defensive.
        ReadinessDecision::PendingNoResult => {
            debug!(
                "Rolling update on hold for {}: still waiting for first readiness check result",
                child.id
            );
            false
        }
        ReadinessDecision::PendingMinHealthyTime { remaining } => {
            debug!(
                "Rolling update on hold for {}: readiness green but need {:?} more in the anti-flap window",
                child.id, remaining
            );
            false
        }
        ReadinessDecision::Failing => {
            debug!(
                "Rolling update on hold for {}: at least one readiness check is failing",
                child.id
            );
            false
        }
    }
}

/// Handle rolling update coordination for deployments that have a `parent_id`.
///
/// Called after `apply_runtime` + `run_health_checks` for each child deployment.
/// - If the child is `Running` (healthy): remove one instance from the parent.
///   When the parent reaches 0 instances, mark it as `Deleted` and clear `parent_id`.
/// - If the child is `Failed`: stop the rollout — parent containers keep running.
async fn handle_rolling_update(
    pool: &SqlitePool,
    child: &mut Deployment,
    deleted: &mut Vec<String>,
    runtime: &dyn RuntimeLifecycle,
) {
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
            Some("rolling_update_failed"),
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

    // Readiness gate. If the child declares any HC with `readiness: true`,
    // we hold off on draining the parent until those checks have been
    // green for at least `min_healthy_time`. Deployments without any
    // readiness HC keep the legacy behaviour (drain on `Running`) so this
    // change is fully opt-in for existing manifests.
    if !is_ready_to_drain(pool, child).await {
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

    // Drain one instance per cycle if the parent still has some. If the
    // remove fails, bail — the next cycle will retry.
    if !parent.instances.is_empty() {
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
            Some("rolling_update_step"),
        )
        .await
        {
            warn!("Failed to log rolling update step event: {}", e);
        }
    }

    // Finalize in the same cycle the last instance was drained: otherwise
    // the parent's `apply_runtime` on the next cycle would see replicas=1
    // vs instances=0 and respawn a fresh container, sending us into a
    // spawn/kill loop that never lets the rollout converge.
    if parent.instances.is_empty() {
        info!(
            "Rolling update complete: parent {} has 0 instances, marking as deleted",
            parent.id
        );
        parent.status = DeploymentStatus::Deleted;
        if let Err(e) = deployments::update(pool, &parent).await {
            error!("Failed to mark parent {} as deleted: {}", parent.id, e);
        }
        deleted.push(parent.id.clone());

        child.parent_id = None;

        if let Err(e) = deployment_event::log_event(
            pool,
            child.id.clone(),
            "info",
            format!(
                "Rolling update complete: replaced parent deployment {}",
                parent_id
            ),
            "scheduler",
            Some("rolling_update_complete"),
        )
        .await
        {
            warn!("Failed to log rolling update complete event: {}", e);
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
async fn drain_docker_events(
    pool: &SqlitePool,
    event_rx: &mut mpsc::Receiver<DockerEvent>,
    intentional_shutdowns: &IntentionalShutdowns,
) {
    loop {
        match event_rx.try_recv() {
            Ok(event) => apply_docker_event(pool, event, intentional_shutdowns).await,
            Err(mpsc::error::TryRecvError::Empty) => return,
            Err(mpsc::error::TryRecvError::Disconnected) => {
                error!("Docker event channel disconnected — listener task likely died");
                return;
            }
        }
    }
}

async fn apply_docker_event(
    pool: &SqlitePool,
    event: DockerEvent,
    intentional_shutdowns: &IntentionalShutdowns,
) {
    match event {
        DockerEvent::ContainerDied {
            deployment_id,
            container_id,
            exit_code,
        } => {
            // Operator-initiated shutdowns (scale-down, delete, rolling update,
            // health-check kill) are pre-marked in `IntentionalShutdowns` by the
            // runtime before it issues the stop. The matching `die` event must
            // therefore NOT bump `restart_count` — otherwise we'd flip a healthy
            // deployment into CrashLoopBackOff just for being scaled down.
            if intentional_shutdowns.take(&container_id).await {
                debug!(
                    "Ignoring die event for {} (intentional shutdown)",
                    container_id
                );
                return;
            }
            bump_restart_count(
                pool,
                &deployment_id,
                format!(
                    "Container {} died (exit_code={})",
                    container_id,
                    exit_code
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "?".to_string()),
                ),
                "ContainerDied",
            )
            .await;
        }
        DockerEvent::ContainerOom {
            deployment_id,
            container_id,
        } => {
            // Docker emits `oom` then `die`; we count on `die` so we don't double-count.
            // This branch only logs the OOM cause for traceability.
            if let Err(e) = deployment_event::log_event(
                pool,
                deployment_id,
                "warning",
                format!("Container {} killed by OOM", container_id),
                "docker-events",
                Some("container_oom"),
            )
            .await
            {
                warn!("Failed to log OOM event: {}", e);
            }
        }
        DockerEvent::ContainerKilled {
            deployment_id,
            container_id,
            signal,
        } => {
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
                Some("container_killed"),
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
            error!(
                "Failed to load deployment {} on event: {}",
                deployment_id, e
            );
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
        "warning",
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
    intentional_shutdowns: IntentionalShutdowns,
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

    // Per-deployment retry backoff. Shared across runtimes so any new runtime
    // (Docker, Cloud Hypervisor, future Firecracker, ...) automatically gets
    // exponential backoff on transient failures without duplicating the logic.
    let mut backoff = RetryBackoff::new();

    info!(
        "Starting scheduler with interval: {}s, apply timeout: {}s",
        interval_seconds, apply_timeout_secs
    );

    loop {
        // Apply any crash events received from Docker since the last cycle.
        // Doing this before `find_all` ensures that the deployments we load
        // already reflect the latest restart_count, so the worker scaler can
        // hit CrashLoopBackOff in the same cycle as the crash that caused it.
        drain_docker_events(&pool, &mut event_rx, &intentional_shutdowns).await;

        // The scheduler picks up every status that can still progress on the
        // next tick. Pending/Creating need their first apply, Running needs
        // reconciliation, Deleted needs cleanup, and the transient error
        // states need to keep retrying until `restart_count` reaches
        // `MAX_RESTART_COUNT` — at which point the runtime flips the
        // deployment to `CrashLoopBackOff` and stops being included here.
        //
        // Statuses left out on purpose: Completed (terminal job), Failed,
        // CrashLoopBackOff (terminal failure). Anything in those states is
        // done — no point reconciling further.
        //
        // We go through `DeploymentStatus::to_string()` rather than writing
        // the literal strings here: the compiler then guarantees the filter
        // stays in sync with the enum's Display impl. A bare string literal
        // would silently drift if the enum casing ever changes again.
        let mut filters = HashMap::new();
        filters.insert(
            String::from("status"),
            vec![
                DeploymentStatus::Pending.to_string(),
                DeploymentStatus::Creating.to_string(),
                DeploymentStatus::Running.to_string(),
                DeploymentStatus::Deleted.to_string(),
                DeploymentStatus::CreateContainerError.to_string(),
                DeploymentStatus::ImagePullBackOff.to_string(),
                DeploymentStatus::NetworkError.to_string(),
                DeploymentStatus::ConfigError.to_string(),
                DeploymentStatus::FileSystemError.to_string(),
                DeploymentStatus::Error.to_string(),
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
                    debug!(
                        "No runtime registered for '{}', skipping deployment {}",
                        deployment.runtime, deployment.id
                    );
                    continue;
                }
            };

            // Honour the retry backoff. Deletes always go through so the
            // user can wipe a stuck deployment without waiting.
            if deployment.status != DeploymentStatus::Deleted && backoff.is_blocked(&deployment.id)
            {
                debug!(
                    "Deployment {} in retry backoff, skipping cycle",
                    deployment.id
                );
                continue;
            }

            let configs = match load_configs(&pool, &deployment).await {
                Some(c) => c,
                None => continue,
            };

            let volume_secrets = match load_secrets_for_volumes(&pool, &deployment).await {
                Some(s) => s,
                None => continue,
            };

            let resolved = match prepare_deployment(&pool, &deployment).await {
                Some(d) => d,
                None => continue,
            };

            let resolved_mounts = match crate::models::volume::resolve_volumes(
                &deployment.volumes,
                &configs,
                &volume_secrets,
            ) {
                Ok(mounts) => mounts,
                Err(e) => {
                    error!(
                        "Failed to resolve volumes for deployment {}: {}",
                        deployment.id, e
                    );
                    // Surface to the operator via `ring deployment events`.
                    // The message contains only the resource name (e.g.
                    // "Secret 'X' not found"), never plaintext values.
                    if let Err(log_err) = deployment_event::log_event(
                        &pool,
                        deployment.id.clone(),
                        "error",
                        format!("Failed to resolve volumes: {}", e),
                        "scheduler",
                        Some("volume_resolution_error"),
                    )
                    .await
                    {
                        warn!("Failed to log volume resolution error event: {}", log_err);
                    }
                    continue;
                }
            };

            let restart_count_before = deployment.restart_count;
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

            // Translate the runtime's outcome into a backoff decision.
            // A bumped restart_count means the runtime hit a transient
            // failure — arm the next retry. Otherwise (success, terminal
            // status, or deleted) drop any pending backoff so the next
            // legitimate failure starts at 1s again.
            if result.restart_count > restart_count_before
                && result.status != DeploymentStatus::CrashLoopBackOff
                && result.status != DeploymentStatus::Failed
            {
                backoff.arm(&result.id, result.restart_count);
            } else {
                backoff.clear(&result.id);
            }

            persist_pending_events(&pool, &mut result).await;

            // Re-read current status from DB to detect concurrent changes (e.g. API delete)
            if let Ok(Some(current)) = deployments::find(&pool, &result.id).await {
                if current.status == DeploymentStatus::Deleted
                    && result.status != DeploymentStatus::Deleted
                {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::health_check::{FailureAction, HealthCheck};
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

    fn child_with_health_checks(id: &str, hcs: Vec<HealthCheck>) -> Deployment {
        Deployment {
            id: id.to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
            updated_at: None,
            status: DeploymentStatus::Running,
            restart_count: 0,
            namespace: "test".to_string(),
            name: "child".to_string(),
            image: "nginx:alpine".to_string(),
            config: None,
            runtime: "docker".to_string(),
            kind: "worker".to_string(),
            replicas: 1,
            command: vec![],
            instances: vec!["instance-1".to_string()],
            labels: HashMap::new(),
            environment: HashMap::new(),
            volumes: "[]".to_string(),
            health_checks: hcs,
            resources: None,
            image_digest: None,
            ports: vec![],
            pending_events: vec![],
            parent_id: Some("parent-id".to_string()),
            network: None,
        }
    }

    /// Insert a health_check row directly so the gate has something to read.
    /// `seconds_ago` is the offset from now() applied to all timestamps —
    /// makes the anti-flap window deterministic without sleeping.
    async fn insert_hc_result(
        pool: &SqlitePool,
        deployment_id: &str,
        check_type: &str,
        status: &str,
        seconds_ago: i64,
    ) {
        let ts = (chrono::Utc::now() - chrono::Duration::seconds(seconds_ago)).to_rfc3339();
        sqlx::query(
            "INSERT INTO health_check (id, deployment_id, check_type, status, message, created_at, started_at, finished_at)
             VALUES (?, ?, ?, ?, NULL, ?, ?, ?)",
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(deployment_id)
        .bind(check_type)
        .bind(status)
        .bind(&ts)
        .bind(&ts)
        .bind(&ts)
        .execute(pool)
        .await
        .expect("insert HC result");
    }

    fn readiness_command(name: &str) -> HealthCheck {
        let _ = name;
        HealthCheck::Command {
            command: "test -f /var/run/kemeter/ready".to_string(),
            interval: "10s".to_string(),
            timeout: "5s".to_string(),
            threshold: 3,
            on_failure: FailureAction::Alert,
            readiness: true,
            min_healthy_time: None,
        }
    }

    fn readiness_tcp() -> HealthCheck {
        HealthCheck::Tcp {
            port: 80,
            interval: "10s".to_string(),
            timeout: "5s".to_string(),
            threshold: 3,
            on_failure: FailureAction::Alert,
            readiness: true,
            min_healthy_time: None,
        }
    }

    fn liveness_command() -> HealthCheck {
        HealthCheck::Command {
            command: "true".to_string(),
            interval: "10s".to_string(),
            timeout: "5s".to_string(),
            threshold: 3,
            on_failure: FailureAction::Alert,
            readiness: false,
            min_healthy_time: None,
        }
    }

    fn child_with_min_healthy(id: &str, hc_min: Vec<(bool, Option<&str>)>) -> Deployment {
        let hcs: Vec<HealthCheck> = hc_min
            .into_iter()
            .map(|(readiness, raw)| HealthCheck::Tcp {
                port: 80,
                interval: "10s".to_string(),
                timeout: "5s".to_string(),
                threshold: 3,
                on_failure: FailureAction::Alert,
                readiness,
                min_healthy_time: raw.map(|s| s.to_string()),
            })
            .collect();
        child_with_health_checks(id, hcs)
    }

    #[test]
    fn min_healthy_time_default_when_no_override() {
        let child = child_with_min_healthy("c", vec![(true, None)]);
        assert_eq!(min_healthy_time_for(&child), DEFAULT_MIN_HEALTHY_TIME);
    }

    #[test]
    fn min_healthy_time_uses_per_hc_value() {
        let child = child_with_min_healthy("c", vec![(true, Some("30s"))]);
        assert_eq!(min_healthy_time_for(&child), Duration::from_secs(30));
    }

    #[test]
    fn min_healthy_time_takes_max_across_readiness_checks() {
        // Two readiness HCs: 15s and 45s — the most-cautious value wins.
        let child = child_with_min_healthy("c", vec![(true, Some("15s")), (true, Some("45s"))]);
        assert_eq!(min_healthy_time_for(&child), Duration::from_secs(45));
    }

    #[test]
    fn min_healthy_time_ignores_non_readiness_hcs() {
        // A non-readiness HC's window must not influence the gate.
        let child = child_with_min_healthy("c", vec![(false, Some("999s")), (true, Some("20s"))]);
        assert_eq!(min_healthy_time_for(&child), Duration::from_secs(20));
    }

    #[test]
    fn min_healthy_time_falls_back_on_malformed_value() {
        let child = child_with_min_healthy("c", vec![(true, Some("not-a-duration"))]);
        assert_eq!(min_healthy_time_for(&child), DEFAULT_MIN_HEALTHY_TIME);
    }

    #[tokio::test]
    async fn drain_allowed_when_no_readiness_hc() {
        // Backward compat: deployments without any readiness HC keep the
        // legacy "drain on Running" behaviour.
        let pool = new_test_pool().await;
        let child = child_with_health_checks("child-1", vec![liveness_command()]);
        assert!(is_ready_to_drain(&pool, &child).await);
    }

    #[tokio::test]
    async fn drain_blocked_when_readiness_has_no_result_yet() {
        let pool = new_test_pool().await;
        let child = child_with_health_checks("child-2", vec![readiness_command("ready")]);
        // No insert into health_check — no result has been recorded yet.
        assert!(!is_ready_to_drain(&pool, &child).await);
    }

    #[tokio::test]
    async fn drain_blocked_when_readiness_too_recent() {
        let pool = new_test_pool().await;
        let child = child_with_health_checks("child-3", vec![readiness_command("ready")]);
        // Success 5s ago, MIN_HEALTHY_TIME is 10s → not enough.
        insert_hc_result(&pool, "child-3", "command", "success", 5).await;
        assert!(!is_ready_to_drain(&pool, &child).await);
    }

    #[tokio::test]
    async fn drain_allowed_when_readiness_old_enough() {
        let pool = new_test_pool().await;
        let child = child_with_health_checks("child-4", vec![readiness_command("ready")]);
        // Success 30s ago — well past MIN_HEALTHY_TIME.
        insert_hc_result(&pool, "child-4", "command", "success", 30).await;
        assert!(is_ready_to_drain(&pool, &child).await);
    }

    #[tokio::test]
    async fn drain_respects_custom_min_healthy_time() {
        // A 60s anti-flap window on the readiness HC: a 30s-old success is
        // not enough anymore. The same data would have unblocked drain with
        // the default 10s window.
        let pool = new_test_pool().await;
        let mut hcs = vec![readiness_command("ready")];
        if let HealthCheck::Command {
            min_healthy_time, ..
        } = &mut hcs[0]
        {
            *min_healthy_time = Some("60s".to_string());
        }
        let child = child_with_health_checks("child-mht", hcs);
        insert_hc_result(&pool, "child-mht", "command", "success", 30).await;
        assert!(
            !is_ready_to_drain(&pool, &child).await,
            "30s should be too recent for a 60s anti-flap window"
        );

        // Bump the recorded success to 90s ago — now past the custom window.
        insert_hc_result(&pool, "child-mht", "command", "success", 90).await;
        assert!(is_ready_to_drain(&pool, &child).await);
    }

    #[tokio::test]
    async fn drain_blocked_when_latest_readiness_failed() {
        let pool = new_test_pool().await;
        let child = child_with_health_checks("child-5", vec![readiness_command("ready")]);
        // An old success and a recent failure — gate must read the latest only.
        insert_hc_result(&pool, "child-5", "command", "success", 60).await;
        insert_hc_result(&pool, "child-5", "command", "failed", 5).await;
        assert!(!is_ready_to_drain(&pool, &child).await);
    }

    #[tokio::test]
    async fn drain_requires_all_readiness_hcs_to_have_a_result() {
        let pool = new_test_pool().await;
        // Two readiness HCs declared (tcp + command) but only one has produced
        // a result. We must wait until both have at least one entry.
        let child =
            child_with_health_checks("child-6", vec![readiness_tcp(), readiness_command("ready")]);
        insert_hc_result(&pool, "child-6", "command", "success", 30).await;
        // tcp has no result yet — gate must hold.
        assert!(!is_ready_to_drain(&pool, &child).await);

        // After both are green and old enough, gate opens.
        insert_hc_result(&pool, "child-6", "tcp", "success", 30).await;
        assert!(is_ready_to_drain(&pool, &child).await);
    }

    #[tokio::test]
    async fn drain_ignores_non_readiness_hc_results() {
        // A non-readiness HC must not influence the decision: even if it has
        // never produced a result, the readiness HC alone gates the drain.
        let pool = new_test_pool().await;
        let mut hcs = vec![readiness_command("ready")];
        hcs.push(HealthCheck::Tcp {
            port: 80,
            interval: "10s".to_string(),
            timeout: "5s".to_string(),
            threshold: 3,
            on_failure: FailureAction::Alert,
            readiness: false, // not a readiness HC
            min_healthy_time: None,
        });
        let child = child_with_health_checks("child-7", hcs);
        insert_hc_result(&pool, "child-7", "command", "success", 30).await;
        // tcp has no result, but it's not a readiness HC, so gate opens.
        assert!(is_ready_to_drain(&pool, &child).await);
    }
}
