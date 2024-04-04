use clap::{Command};
use clap::ArgMatches;
use std::fs;
use crate::config::config::get_config_dir;

pub(crate) fn command_config() -> Command {
    Command::new("init")
        .about("Initialize configuration")
}

pub(crate) fn init(_args: &ArgMatches) {

    fs::create_dir_all(get_config_dir()).expect("Unable to create config directory");
    fs::write(format!("{}/auth.json", get_config_dir()), "{}").expect("Unable to create auth.json");
}
