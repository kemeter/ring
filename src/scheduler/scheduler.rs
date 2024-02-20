use std::collections::HashMap;
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
            deployments::find_all(&guard, HashMap::new())
        };

        let mut deleted:Vec<String> = Vec::new();

        for deployment in list_deployments.into_iter() {
            if "docker" == deployment.runtime {
                let config = docker::apply(deployment.clone()).await;

                if "deleted" == config.status && config.instances.len() == 0 {
                    deleted.push(config.id);
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
