use clap::{Command};
use clap::ArgMatches;
use rusqlite::Connection;
use crate::api::server as ApiServer;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::task;

use crate::scheduler::scheduler::schedule;
use crate::config::config::Config;

pub(crate) fn command_config<'a, 'b>() -> Command {
    Command::new("start")
}

pub(crate) async fn execute(_args: &ArgMatches, configuration: Config, mut storage: Connection) {
/*    embedded::migrations::runner()
        .run(&mut storage)
        .expect("Could not execute database migrations.");*/

    let connection = Arc::new(Mutex::new(storage));
    let api_server_handler = task::spawn(ApiServer::start(Arc::clone(&connection), configuration));
    let scheduler_handler = task::spawn(schedule(Arc::clone(&connection)));

    let _ = api_server_handler.await;
    let _ = scheduler_handler.await;
}

/*mod embedded {
    refinery::embed_migrations!("src/migrations");
}*/
