use crate::api::server as ApiServer;
use crate::runtime::cloud_hypervisor::CloudHypervisorLifecycle;
use crate::runtime::docker;
use crate::runtime::docker::docker_lifecycle::DockerLifecycle;
use crate::runtime::lifecycle_trait::RuntimeLifecycle;
use clap::ArgMatches;
use clap::Command;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::task;

use crate::config::config::Config;
use crate::database::{get_database_pool, migrate_from_refinery_if_needed};
use crate::scheduler::scheduler::schedule;

pub(crate) fn command_config() -> Command {
    Command::new("start")
}

pub(crate) async fn execute(_args: &ArgMatches, configuration: Config) {
    let pool = get_database_pool().await;

    migrate_from_refinery_if_needed(&pool).await;

    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .expect("Could not execute database migrations.");

    let docker = docker::connect().expect("Failed to connect to Docker");
    info!("Connected to Docker");

    let mut runtimes_map: HashMap<String, Arc<dyn RuntimeLifecycle>> = HashMap::new();
    runtimes_map.insert("docker".to_string(), Arc::new(DockerLifecycle::new(docker.clone())));
    runtimes_map.insert(
        "cloud-hypervisor".to_string(),
        Arc::new(CloudHypervisorLifecycle::new(Default::default())),
    );
    info!("Registered runtimes: {:?}", runtimes_map.keys().collect::<Vec<_>>());

    let runtimes = std::sync::Arc::new(runtimes_map.clone());

    let api_server_handler = task::spawn(ApiServer::start(pool.clone(), configuration.clone(), docker.clone(), runtimes.clone()));
    let scheduler_handler = task::spawn(schedule(pool, configuration, runtimes_map, docker));

    if let Err(e) = api_server_handler.await {
        eprintln!("API server task failed: {}", e);
    }
    if let Err(e) = scheduler_handler.await {
        eprintln!("Scheduler task failed: {}", e);
    }
}
