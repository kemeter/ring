use std::collections::HashMap;
use crate::runtime::docker;
use crate::models::deployments;
use crate::models::config;
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
                String::from("running")
            ]);
            deployments::find_all(&guard, HashMap::new())
        };

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
                        eprintln!("Erreur : {}", e);
                        return; // ou gérer l'erreur selon votre contexte
                    }
                };


                let mut config = docker::apply(deployment.clone(), configs).await;

                if "deleted" == config.status && config.instances.len() == 0 {
                    deleted.push(config.id.clone());
                }

                {
                    if config.status == "creating" && config.instances.len() > 0 {
                        config.status = "running".to_string();
                    }

                    let guard = storage.lock().await;
                    deployments::update(&guard, &config);
                }
            }
        }

        {
            let guard = storage.lock().await;
            deployments::delete_batch(&guard, deleted);
        }


        sleep(duration).await;
    }
}
