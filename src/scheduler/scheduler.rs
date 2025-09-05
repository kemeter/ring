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

pub(crate) async fn schedule(storage: Arc<Mutex<Connection>>) {
    let duration = Duration::from_secs(10);

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

                    let guard = storage.lock().await;
                    deployments::update(&guard, &config);
                }
            }
        }

        if !deleted.is_empty() {
            info!("Cleaning up {} deployments", deleted.len());
            let guard = storage.lock().await;
            
            // Clean up deployment events before deleting deployments
            for id in &deleted {
                if let Ok(count) = deployment_event::delete_by_deployment_id(&guard, id) {
                    debug!("Deleted {} events for deployment {}", count, id);
                }
            }
            
            deployments::delete_batch(&guard, deleted);
        }

        debug!("Scheduler cycle completed, sleeping for {}s", duration.as_secs());
        sleep(duration).await;
    }
}
