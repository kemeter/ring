use clap::App;
use clap::SubCommand;
use clap::ArgMatches;
use std::fs;
use rusqlite::Connection;

pub(crate) fn command_config<'a, 'b>() -> App<'a, 'b> {
    SubCommand::with_name("init")
        .name("init")
}

pub(crate) fn init(_args: &ArgMatches, connection: Connection) {

    let migration_init = fs::read_to_string("./src/migrations/00.init.sql").unwrap();
    connection.execute(&migration_init, []).unwrap();
}
