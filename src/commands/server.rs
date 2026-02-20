use clap::{Command};
use clap::ArgMatches;
use crate::api::server as ApiServer;
use tokio::task;

use crate::scheduler::scheduler::schedule;
use crate::config::config::Config;
use crate::database::{get_database_pool, migrate_from_refinery_if_needed};

pub(crate) fn command_config<'a, 'b>() -> Command {
    Command::new("start")
}

pub(crate) async fn execute(_args: &ArgMatches, configuration: Config) {
    let pool = get_database_pool().await;

    migrate_from_refinery_if_needed(&pool).await;

    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .expect("Could not execute database migrations.");

    let api_server_handler = task::spawn(ApiServer::start(pool.clone(), configuration.clone()));
    let scheduler_handler = task::spawn(schedule(pool, configuration));

    let _ = api_server_handler.await;
    let _ = scheduler_handler.await;
}
