use crate::runtime::docker;
use crate::models::deployments;
use rusqlite::Connection;
use std::sync::Arc;
use tokio::sync::Mutex;
use std::{thread, time};

pub(crate) async fn schedule(storage: Arc<Mutex<Connection>>) {
    let duration = time::Duration::from_secs(10);

    info!("Starting schedule");

    loop {
        let guard = storage.lock().await;

        let list_deployments = deployments::find_all(guard);
        for deployment in list_deployments.into_iter() {

            if "docker" == deployment.runtime {
                docker::apply(deployment.clone()).await;
            }

            debug!("{:?}", deployment);
        }

        thread::sleep(duration);
    }
}
