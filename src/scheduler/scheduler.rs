use std::collections::HashMap;
use crate::runtime::docker;
use crate::runtime::runtime::Runtime;
use crate::models::deployments::{self, DeploymentStatus};
use crate::models::config;
use crate::models::deployment_event;
use crate::models::health_check_logs;
use crate::scheduler::health_checker::HealthChecker;
use sqlx::SqlitePool;
use std::env;
use tokio::time::{sleep, Duration, Instant};
use crate::models::config::Config;

pub(crate) async fn schedule(pool: SqlitePool, config: crate::config::config::Config) {
    let interval_seconds = env::var("SCHEDULER_INTERVAL")
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

    info!("Starting scheduler with interval: {}s, apply timeout: {}s", interval_seconds, apply_timeout_secs);

    loop {
        let mut filters = HashMap::new();
        filters.insert(String::from("status"), vec![
            String::from("creating"),
            String::from("running"),
            String::from("deleted")
        ]);
        let list_deployments = deployments::find_all(&pool, filters).await;

        info!("Processing {} deployments", list_deployments.len());
        let mut deleted:Vec<String> = Vec::new();

        for deployment in list_deployments.into_iter() {
            if "docker" == deployment.runtime {
                let configs_vec = config::find_by_namespace(&pool, deployment.namespace.clone()).await;

                let configs: HashMap<String, Config> = match configs_vec {
                    Ok(configs_vec) => {
                        configs_vec
                            .into_iter()
                            .map(|config| (config.name.clone(), config))
                            .collect()
                    },
                    Err(e) => {
                        error!("Failed to load configs for deployment {}: {}", deployment.id, e);

                        let _ = deployment_event::log_event(
                            &pool,
                            deployment.id.clone(),
                            "error",
                            format!("Failed to load configs: {}", e),
                            "scheduler",
                            Some("ConfigLoadError")
                        ).await;

                        continue;
                    }
                };

                let mut config = match tokio::time::timeout(apply_timeout, docker::apply(deployment.clone(), configs)).await {
                    Ok(result) => result,
                    Err(_) => {
                        error!("docker::apply timed out for deployment {}", deployment.id);
                        let _ = deployment_event::log_event(
                            &pool,
                            deployment.id.clone(),
                            "error",
                            format!("Scheduler apply timed out after {} seconds", apply_timeout_secs),
                            "scheduler",
                            Some("ApplyTimeout")
                        ).await;
                        continue;
                    }
                };

                // Persist any events emitted by the runtime
                if !config.pending_events.is_empty() {
                    for event in &config.pending_events {
                        let _ = deployment_event::create_event(&pool, event).await;
                    }
                    config.pending_events.clear();
                }

                if config.status == DeploymentStatus::Deleted && config.instances.is_empty() {
                    info!("Marking deployment {} for cleanup", config.id);

                    let _ = deployment_event::log_event(
                        &pool,
                        config.id.clone(),
                        "info",
                        "Deployment marked for cleanup - all containers stopped".to_string(),
                        "scheduler",
                        Some("CleanupScheduled")
                    ).await;

                    deleted.push(config.id.clone());
                }

                if config.status == DeploymentStatus::Creating && !config.instances.is_empty() {
                    info!("Deployment {} transition: creating -> running", config.id);

                    let _ = deployment_event::log_event(
                        &pool,
                        config.id.clone(),
                        "info",
                        format!("Status changed from creating to running ({} containers)", config.instances.len()),
                        "scheduler",
                        Some("StateTransition")
                    ).await;

                    config.status = DeploymentStatus::Running;
                }

                // Execute health checks for running deployments
                if config.status == DeploymentStatus::Running && !config.health_checks.is_empty() {
                    debug!("Executing health checks for deployment {}", config.id);
                    let outcome = health_checker.execute_checks(&config).await;

                    // Persist health check results
                    for result in &outcome.results {
                        health_checker.store_result(result).await;
                    }

                    // Persist events
                    for event in &outcome.events {
                        let _ = deployment_event::create_event(&pool, event).await;
                    }

                    // Apply status change
                    if let Some(new_status) = outcome.proposed_status {
                        config.status = new_status;
                    }

                    // Remove failing instances
                    if !outcome.instances_to_remove.is_empty() {
                        if let Ok(rt) = Runtime::new(config.clone()) {
                            for instance_id in &outcome.instances_to_remove {
                                rt.remove_instance(instance_id).await;
                                config.instances.retain(|id| id != instance_id);
                            }
                        }
                    }
                }

                if let Err(e) = deployments::update(&pool, &config).await {
                    error!("Failed to update deployment {}: {}", config.id, e);
                }
            }
        }

        if !deleted.is_empty() {
            info!("Cleaning up {} deployments", deleted.len());

            for id in &deleted {
                if let Ok(count) = deployment_event::delete_by_deployment_id(&pool, id).await {
                    debug!("Deleted {} events for deployment {}", count, id);
                }

                if let Ok(count) = health_check_logs::delete_by_deployment_id(&pool, id).await {
                    debug!("Deleted {} health checks for deployment {}", count, id);
                }
            }

            if let Err(e) = deployments::delete_batch(&pool, deleted).await {
                error!("Failed to delete deployments: {}", e);
            }
        }

        // Cleanup health checks based on elapsed time
        if last_cleanup.elapsed() >= cleanup_interval {
            last_cleanup = Instant::now();
            if let Err(e) = health_check_logs::cleanup_old_health_checks(&pool).await {
                error!("Failed to cleanup old health checks: {}", e);
            }
        }

        debug!("Scheduler cycle completed, sleeping for {}s", duration.as_secs());
        sleep(duration).await;
    }
}
