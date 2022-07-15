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

        let list_deployments = {
            let guard = storage.lock().await;

            deployments::find_all(guard)
        };

        for deployment in list_deployments.into_iter() {

            if "docker" == deployment.runtime {
                docker::apply(deployment.clone()).await;
            }

            debug!("{:?}", deployment);
        }

        sleep(duration).await;
    }
}
