use crate::runtime::docker;
use crate::models::deployments;
use rusqlite::Connection;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::time::{sleep, Duration};

pub(crate) async fn schedule(storage: Arc<Mutex<Connection>>) {
    let duration = Duration::from_secs(10);

    info!("Starting schedule");

    loop {
        let list_deployments =  {
            let guard = storage.lock().await;
            deployments::find_all(&guard)
        };

        let mut deleted:Vec<String> = Vec::new();

        for deployment in list_deployments.into_iter() {
            if "docker" == deployment.runtime {
                let mut config = docker::apply(deployment.clone()).await;
                config.restart += 1;

                if "deleted" == config.status && config.instances.len() == 0 {
                    deleted.push(config.id.to_string());
                }

                {
                    let guard = storage.lock().await;
                    deployments::update(&guard, &config)
                };
            }
        }

        let guard = storage.lock().await;
        deployments::delete_batch(&guard, deleted);

        sleep(duration).await;
    }
}
