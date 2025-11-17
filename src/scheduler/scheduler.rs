use std::collections::HashMap;
use crate::runtime::docker;
use crate::models::deployments;
use crate::models::config;
use crate::models::deployment_event;
use crate::models::health_check_logs;
use crate::scheduler::health_checker::HealthChecker;
use rusqlite::Connection;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::time::{sleep, Duration};
use crate::models::config::Config;

pub(crate) async fn schedule(storage: Arc<Mutex<Connection>>) {
    let duration = Duration::from_secs(10);
    let health_checker = HealthChecker::new(storage.clone());
    let mut cleanup_counter = 0;

    info!("Starting schedule");

    loop {
        let list_deployments =  {
            let guard = storage.lock().await;
            let mut filters = HashMap::new();
            filters.insert(String::from("status"), vec![
                String::from("creating"),
                String::from("running"),
                String::from("deleted")
            ]);
            deployments::find_all(&guard, filters)
        };

        info!("Processing {} deployments", list_deployments.len());
        let mut deleted:Vec<String> = Vec::new();

        for deployment in list_deployments.into_iter() {
            if "docker" == deployment.runtime {
                let configs_vec = {
                    let guard = storage.lock().await;
                    config::find_by_namespace(&guard, deployment.namespace.clone())
                };

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
                        {
                            let guard = storage.lock().await;
                            let _ = deployment_event::log_event(
                                &guard,
                                deployment.id.clone(),
                                "error",
                                format!("Failed to load configs: {}", e),
                                "scheduler",
                                Some("ConfigLoadError")
                            );
                        }
                        
                        continue; // Skip this deployment, process others
                    }
                };


                let mut config = docker::apply(deployment.clone(), configs).await;

                if "deleted" == config.status && config.instances.len() == 0 {
                    info!("Marking deployment {} for cleanup", config.id);
                    
                    // Record cleanup event
                    {
                        let guard = storage.lock().await;
                        let _ = deployment_event::log_event(
                            &guard,
                            config.id.clone(),
                            "info",
                            "Deployment marked for cleanup - all containers stopped".to_string(),
                            "scheduler",
                            Some("CleanupScheduled")
                        );
                    }
                    
                    deleted.push(config.id.clone());
                }

                {
                    if config.status == "creating" && config.instances.len() > 0 {
                        info!("Deployment {} transition: creating -> running", config.id);
                        
                        // Record state transition event
                        {
                            let guard = storage.lock().await;
                            let _ = deployment_event::log_event(
                                &guard,
                                config.id.clone(),
                                "info",
                                format!("Status changed from creating to running ({} containers)", config.instances.len()),
                                "scheduler",
                                Some("StateTransition")
                            );
                        }
                        
                        config.status = "running".to_string();
                    }

                    // Execute health checks for running deployments
                    if config.status == "running" && !config.health_checks.is_empty() {
                        debug!("Executing health checks for deployment {}", config.id);
                        health_checker.execute_checks(&config).await;
                        
                        // Re-read deployment after health checks to preserve any status changes
                        let guard = storage.lock().await;
                        match deployments::find(&guard, config.id.clone()) {
                            Ok(Some(mut updated_deployment)) => {
                                // Preserve scheduler changes (instances, etc.) but keep health checker status changes
                                updated_deployment.instances = config.instances;
                                updated_deployment.restart_count = config.restart_count;
                                // Keep the status from health checker (could be "deleted" or "failed")
                                deployments::update(&guard, &updated_deployment);
                            }
                            _ => {
                                // If deployment not found, use the original config
                                deployments::update(&guard, &config);
                            }
                        }
                    } else {
                        let guard = storage.lock().await;
                        deployments::update(&guard, &config);
                    }
                }
            }
        }

        if !deleted.is_empty() {
            info!("Cleaning up {} deployments", deleted.len());
            let guard = storage.lock().await;
            
            // Clean up deployment events and health checks before deleting deployments
            for id in &deleted {
                if let Ok(count) = deployment_event::delete_by_deployment_id(&guard, id) {
                    debug!("Deleted {} events for deployment {}", count, id);
                }
                
                // Clean up health checks
                if let Ok(count) = health_check_logs::delete_by_deployment_id(&guard, id) {
                    debug!("Deleted {} health checks for deployment {}", count, id);
                }
            }
            
            deployments::delete_batch(&guard, deleted);
        }

        // Cleanup health checks every 30 cycles (5 minutes with 10s intervals)
        cleanup_counter += 1;
        if cleanup_counter >= 30 {
            cleanup_counter = 0;
            let guard = storage.lock().await;
            if let Err(e) = health_check_logs::cleanup_old_health_checks(&guard) {
                error!("Failed to cleanup old health checks: {}", e);
            }
        }

        debug!("Scheduler cycle completed, sleeping for {}s", duration.as_secs());
        sleep(duration).await;
    }
}
