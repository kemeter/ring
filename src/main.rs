use std::process::Command;
use std::env;
use clap::App;

#[macro_use]
extern crate log;
extern crate env_logger;
extern crate ureq;

mod commands {
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
}

mod database;

use crate::database::get_database_connection;

#[tokio::main]
async fn main() {
    env_logger::init();

    let config = config::config::load_config();

    let commands = vec![
        crate::commands::init::command_config(),
        crate::commands::server::command_config(),
        crate::commands::apply::command_config(),
        crate::commands::login::command_config(),
        crate::commands::deployment::list::command_config(),
        crate::commands::deployment::inspect::command_config(),
        crate::commands::user::list::command_config(),
    ];

    let app = App::new("ring")
      .version("0.1.0")
      .author("Mlanawo Mbechezi <mlanawo.mbechezi@kemeter.io>")
      .about("The ring to rule them all")
      .subcommands(commands);

    let matches = app.get_matches();
    let subcommand_name = matches.subcommand_name();
    let storage = get_database_connection();

    match subcommand_name {
        Some("init") => {
            crate::commands::init::init(
                matches.subcommand_matches("init").unwrap(),
                storage
            );
        }
        Some("server:start") => {
            crate::commands::server::execute(
                matches.subcommand_matches("server:start").unwrap(),
                config,
                storage
            ).await
        }
        Some("apply") => {
          crate::commands::apply::apply(
              matches.subcommand_matches("apply").unwrap(),
              config,
          );
        }
        Some("deployment:list") => {
            crate::commands::deployment::list::execute(
                matches.subcommand_matches("deployment:list").unwrap(),
                config,
            );
        }
        Some("deployment:inspect") => {
            crate::commands::deployment::inspect::execute(
                matches.subcommand_matches("deployment:inspect").unwrap(),
                config
            ).await
        }
        Some("login") => {
            crate::commands::login::execute(
                matches.subcommand_matches("login").unwrap(),
                config,
            );
        }
        Some("user:list") => {
            crate::commands::user::list::execute(
                matches.subcommand_matches("user:list").unwrap(),
                config
            );
        }
        _ => {
            let process_args: Vec<String> = env::args().collect();
            let process_name = process_args[0].as_str().to_owned();

            let mut subprocess = Command::new(process_name.as_str())
                .arg("--help")
                .spawn()
                .expect("failed to execute process");

            subprocess
                .wait()
                .expect("failed to wait for process");
        }
    }
}