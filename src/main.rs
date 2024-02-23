use std::process::Command as BaseCommand;
use std::env;
use clap::{Command, Arg};

#[macro_use]
extern crate log;
extern crate env_logger;
extern crate ureq;

mod commands {
  pub(crate) mod config;
  pub(crate) mod init;
  pub(crate) mod server;
  pub(crate) mod apply;
  pub(crate) mod deployment;
  pub(crate) mod login;
  pub(crate) mod user;
}

mod scheduler {
  pub(crate) mod scheduler;
}

mod runtime {
  pub(crate) mod docker;
}

mod models {
  pub(crate) mod deployments;
  pub(crate) mod users;
}

mod api;

mod config {
    pub(crate) mod api;
    pub(crate) mod config;
    pub(crate) mod user;
}

mod database;

use crate::database::get_database_connection;

#[tokio::main]
async fn main() {
    env_logger::init();

    let commands = vec![
        commands::config::command_config(),
        commands::init::command_config(),
        commands::server::command_config(),
        commands::apply::command_config(),
        commands::login::command_config(),
        commands::deployment::list::command_config(),
        commands::deployment::inspect::command_config(),
        commands::deployment::delete::command_config(),
        commands::user::list::command_config(),
        commands::user::create::command_config(),
        commands::user::update::command_config(),
        commands::user::delete::command_config(),
    ];

    let app = Command::new("ring")
        .version("0.1.0")
        .author("Mlanawo Mbechezi <mlanawo.mbechezi@kemeter.io>")
        .about("The ring to rule them all")
        .arg(
            Arg::new("context")
                .required(false)
                .help("Sets the context to use")
                .long("context")
                .short('c')
        )
      .subcommands(commands);

    let matches = app.get_matches();
    let subcommand_name = matches.subcommand();
    let storage = get_database_connection();
    let config = config::config::load_config();

    match subcommand_name {
        Some(("config", sub_matches)) => {
            commands::config::execute(
                sub_matches,
                config,
            );
        }
        Some(("init", sub_matches)) => {
            commands::init::init(
                sub_matches,
                storage
            );
        }
        Some(("server:start", sub_matches)) => {
            commands::server::execute(
                sub_matches,
                config,
                storage
            ).await
        }
        Some(("apply", sub_matches)) => {
          commands::apply::apply(
              sub_matches,
              config,
          );
        }
        Some(("deployment:list", sub_matches)) => {
            commands::deployment::list::execute(
                sub_matches,
                config,
            );
        }
        Some(("deployment:inspect", sub_matches)) => {
            commands::deployment::inspect::execute(
                sub_matches,
                config
            ).await
        }
        Some(("deployment:delete", sub_matches)) => {
            commands::deployment::delete::execute(
                sub_matches,
                config
            ).await
        }
        Some(("login", sub_matches)) => {
            commands::login::execute(
                sub_matches,
                config,
            );
        }
        Some(("user:list", sub_matches)) => {
            commands::user::list::execute(
                sub_matches,
                config
            );
        }
        Some(("user:create", sub_matches)) => {
            commands::user::create::execute(
                sub_matches,
                config
            );
        }
        Some(("user:update", sub_matches)) => {
            commands::user::update::execute(
                sub_matches,
                config
            );
        }
        Some(("user:delete", sub_matches)) => {
            commands::user::delete::execute(
                sub_matches,
                config
            );
        }
        _ => {
            let process_args: Vec<String> = env::args().collect();
            let process_name = process_args[0].as_str().to_owned();

            let mut subprocess = BaseCommand::new(process_name.as_str())
                .arg("--help")
                .spawn()
                .expect("failed to execute process");

            subprocess
                .wait()
                .expect("failed to wait for process");
        }
    }
}

