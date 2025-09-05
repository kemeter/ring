use std::collections::HashMap;
use crate::runtime::docker;
use crate::models::deployments;
use crate::models::config;
use crate::models::deployment_event;
use rusqlite::Connection;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::time::{sleep, Duration};
use crate::models::config::Config;
use log::{info, debug, error};

pub(crate) async fn schedule(storage: Arc<Mutex<Connection>>) {
    let duration = Duration::from_secs(10);

    info!("Starting schedule");

    loop {
        let list_deployments =  {
            let guard = storage.lock().await;
            let mut filters = HashMap::new();
            filters.insert(String::from("status"), vec![
                String::from("creating"),
                String::from("active"),
                String::from("superseded"),
                String::from("deleted")
            ]);
            deployments::find_all(&guard, filters)
        };

        info!("Processing {} deployments", list_deployments.len());
        let mut deleted:Vec<String> = Vec::new();

        for deployment in list_deployments.into_iter() {
            // Skip superseded deployments - they should not be actively managed
            if deployment.status == "superseded" {
                continue;
            }
            
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
                        info!("Deployment {} transition: creating -> active", config.id);
                        
                        // Blue/Green transition: Mark as active and supersede predecessor
                        config.status = "active".to_string();
                        
                        let guard = storage.lock().await;
                        deployments::update(&guard, &config);
                        
                        // Supersede the predecessor deployment (Blue/Green switch)
                        if let Err(e) = deployments::supersede_predecessor(&guard, &config.id) {
                            error!("Failed to supersede predecessor for deployment {}: {}", config.id, e);
                            
                            let _ = deployment_event::log_event(
                                &guard,
                                config.id.clone(),
                                "error",
                                format!("Failed to supersede predecessor: {}", e),
                                "scheduler",
                                Some("SupersedeError")
                            );
                        } else {
                            // Record successful Blue/Green transition
                            let _ = deployment_event::log_event(
                                &guard,
                                config.id.clone(),
                                "info",
                                format!("Blue/Green transition completed - deployment is now active ({} containers)", config.instances.len()),
                                "scheduler",
                                Some("BlueGreenTransition")
                            );
                        }
                    } else if config.status == "creating" && config.instances.len() == 0 {
                        // Handle deployment failure - rollback if possible
                        info!("Deployment {} failed to start, attempting rollback", config.id);
                        
                        let guard = storage.lock().await;
                        
                        match deployments::rollback_to_predecessor(&guard, &config.id) {
                            Ok(Some(predecessor_id)) => {
                                info!("Rolled back failed deployment {} to predecessor {}", config.id, predecessor_id);
                                
                                let _ = deployment_event::log_event(
                                    &guard,
                                    predecessor_id.clone(),
                                    "info",
                                    format!("Automatic rollback from failed deployment {}", config.id),
                                    "scheduler",
                                    Some("AutomaticRollback")
                                );
                            }
                            Ok(None) => {
                                // No predecessor to rollback to, mark as failed
                                config.status = "failed".to_string();
                                deployments::update(&guard, &config);
                                
                                let _ = deployment_event::log_event(
                                    &guard,
                                    config.id.clone(),
                                    "error",
                                    "Deployment failed and no predecessor available for rollback".to_string(),
                                    "scheduler",
                                    Some("DeploymentFailed")
                                );
                            }
                            Err(e) => {
                                error!("Failed to rollback deployment {}: {}", config.id, e);
                                config.status = "failed".to_string();
                                deployments::update(&guard, &config);
                                
                                let _ = deployment_event::log_event(
                                    &guard,
                                    config.id.clone(),
                                    "error",
                                    format!("Deployment failed and rollback failed: {}", e),
                                    "scheduler",
                                    Some("RollbackFailed")
                                );
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
            deployments::delete_batch(&guard, deleted);
        }

        debug!("Scheduler cycle completed, sleeping for {}s", duration.as_secs());
        sleep(duration).await;
    }
}
