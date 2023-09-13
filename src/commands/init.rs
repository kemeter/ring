use clap::App;
use clap::SubCommand;
use clap::ArgMatches;
use std::fs;
use rusqlite::Connection;
use crate::config::config::get_config_dir;

pub(crate) fn command_config<'a, 'b>() -> App<'a, 'b> {
    SubCommand::with_name("init")
        .name("init")
}

pub(crate) fn init(_args: &ArgMatches, connection: Connection) {

    fs::create_dir_all(get_config_dir()).expect("Unable to create config directory");
    fs::write(format!("{}/auth.json", get_config_dir()), "{}").expect("Unable to create auth.json");
}
