use clap::App;
use clap::SubCommand;
use log::info;
use clap::ArgMatches;
use rusqlite::Connection;
use crate::api::server as ApiServer;
use std::thread;
use std::sync::{Mutex, Arc};

use crate::scheduler::scheduler::schedule;

pub(crate) fn command_config<'a, 'b>() -> App<'a, 'b> {
    SubCommand::with_name("server:start")
        .name("server:start")
}

pub(crate) fn server(_args: &ArgMatches, storage: Connection) {
    info!("Start server");
    println!("Start server");

    let connection = Arc::new(Mutex::new(storage));
    let arc = Arc::clone(&connection);
    let arc2 = Arc::clone(&connection);

    let handle = thread::spawn(move || {
        let server_address = "127.0.0.1:8080";
        ApiServer::start(arc, server_address);
    });

    let jq = thread::spawn(move || {
        schedule(arc2);
    });

    handle.join().unwrap();
    jq.join().unwrap();
}
