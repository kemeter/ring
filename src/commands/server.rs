use clap::App;
use clap::SubCommand;
use log::info;
use clap::ArgMatches;
use rusqlite::Connection;
use crate::api::server as ApiServer;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::task;

use crate::scheduler::scheduler::schedule;
use crate::config::config::Config;

pub(crate) fn command_config<'a, 'b>() -> App<'a, 'b> {
    SubCommand::with_name("server:start")
        .name("server:start")
}

pub(crate) async fn execute(_args: &ArgMatches<'_>, configuration: Config, storage: Connection) {
    info!("Start server");
    println!("Start server");

    let connection = Arc::new(Mutex::new(storage));

    let arc = Arc::clone(&connection);
    let arc2 = Arc::clone(&connection);

    let api_server_handler = task::spawn(async move {
        ApiServer::start(arc, configuration).await;
    });

    // let scheduler_handler = task::spawn(async move {
    //     schedule(arc2).await;
    // });

    api_server_handler.await;
    // scheduler_handler.await;
}
