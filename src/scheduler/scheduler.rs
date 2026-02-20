use std::collections::HashMap;
use crate::runtime::docker;
use crate::models::deployments::{self, DeploymentStatus};
use crate::models::config;
use crate::models::deployment_event;
use crate::models::health_check_logs;
use crate::scheduler::health_checker::HealthChecker;
use sqlx::SqlitePool;
use std::env;
use tokio::time::{sleep, Duration};
use crate::models::config::Config;

pub(crate) async fn schedule(pool: SqlitePool, config: crate::config::config::Config) {
    let interval_seconds = env::var("SCHEDULER_INTERVAL")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(config.scheduler.interval);

    let duration = Duration::from_secs(interval_seconds);
    let health_checker = HealthChecker::new(pool.clone());
    let mut cleanup_counter = 0;

    info!("Starting scheduler with interval: {}s", interval_seconds);

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

                        // Record error event
                        let _ = deployment_event::log_event(
                            &pool,
                            deployment.id.clone(),
                            "error",
                            format!("Failed to load configs: {}", e),
                            "scheduler",
                            Some("ConfigLoadError")
                        ).await;

                        continue; // Skip this deployment, process others
                    }
                };


                let mut config = docker::apply(deployment.clone(), configs).await;

                // Persist any events emitted by the runtime
                if !config.pending_events.is_empty() {
                    for event in &config.pending_events {
                        let _ = deployment_event::create_event(&pool, event).await;
                    }
                    config.pending_events.clear();
                }

                if config.status == DeploymentStatus::Deleted && config.instances.is_empty() {
                    info!("Marking deployment {} for cleanup", config.id);

                    // Record cleanup event
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

                {
                    if config.status == DeploymentStatus::Creating && !config.instances.is_empty() {
                        info!("Deployment {} transition: creating -> running", config.id);

                        // Record state transition event
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
                        health_checker.execute_checks(&config).await;

                        // Re-read deployment after health checks to preserve any status changes
                        match deployments::find(&pool, config.id.clone()).await {
                            Ok(Some(mut updated_deployment)) => {
                                // Preserve scheduler changes (instances, etc.) but keep health checker status changes
                                updated_deployment.instances = config.instances;
                                updated_deployment.restart_count = config.restart_count;
                                // Keep the status from health checker (could be "deleted" or "failed")
                                deployments::update(&pool, &updated_deployment).await;
                            }
                            _ => {
                                // If deployment not found, use the original config
                                deployments::update(&pool, &config).await;
                            }
                        }
                    } else {
                        deployments::update(&pool, &config).await;
                    }
                }
            }
        }

        if !deleted.is_empty() {
            info!("Cleaning up {} deployments", deleted.len());

            // Clean up deployment events and health checks before deleting deployments
            for id in &deleted {
                if let Ok(count) = deployment_event::delete_by_deployment_id(&pool, id).await {
                    debug!("Deleted {} events for deployment {}", count, id);
                }

                // Clean up health checks
                if let Ok(count) = health_check_logs::delete_by_deployment_id(&pool, id).await {
                    debug!("Deleted {} health checks for deployment {}", count, id);
                }
            }

            deployments::delete_batch(&pool, deleted).await;
        }

        // Cleanup health checks every 30 cycles (5 minutes with 10s intervals)
        cleanup_counter += 1;
        if cleanup_counter >= 30 {
            cleanup_counter = 0;
            if let Err(e) = health_check_logs::cleanup_old_health_checks(&pool).await {
                error!("Failed to cleanup old health checks: {}", e);
            }
        }

        debug!("Scheduler cycle completed, sleeping for {}s", duration.as_secs());
        sleep(duration).await;
    }
}
