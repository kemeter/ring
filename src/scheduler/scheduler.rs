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
        let guard = storage.lock().await;
        let list_deployments = deployments::find_all(&guard);

        for deployment in list_deployments.into_iter() {
            let config = deployment.clone();

            if "docker" == deployment.runtime {
                let instances = docker::list_instances(deployment.id.to_string()).await;

                if "deleted" == deployment.status && instances.len() == 0 {
                    deployments::delete(&guard, config.id);
                }

                docker::apply(deployment.clone()).await;
            }

            debug!("{:?}", deployment);
        }

        sleep(duration).await;
    }
}
