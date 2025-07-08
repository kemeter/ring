use std::process::Command as BaseCommand;
use std::env;
use clap::{Command, Arg};

#[macro_use]
extern crate log;
extern crate env_logger;
extern crate ureq;
mod commands {
  pub(crate) mod context;
  pub(crate) mod init;
  pub(crate) mod server;
  pub(crate) mod apply;
  pub(crate) mod deployment;

  pub(crate) mod namespace;
  pub(crate) mod node;
  pub(crate) mod login;
  pub(crate) mod user;
  pub(crate) mod config;
}

mod scheduler {
  pub(crate) mod scheduler;
}

mod runtime {
  pub(crate) mod docker;
  pub(crate) mod runtime;
}

mod models {
  pub(crate) mod deployments;
  pub(crate) mod users;
  pub(crate) mod config;
}

mod api;

mod config {
    pub(crate) mod api;
    pub(crate) mod config;
    pub(crate) mod user;
}

mod database;
mod serializer;

use crate::database::get_database_connection;

#[tokio::main]
async fn main() {
    env_logger::init();

    let app = Command::new("ring")
        .version("0.1.0")
        .author("Mlanawo Mbechezi <mlanawo.mbechezi@kemeter.io>")
        .about("The ring to rule them all")
        .arg(
            Arg::new("context")
                .required(false)
                .help("Sets the context to use (e.g., development, staging, production)")
                .long("context")
                .short('c')
        )
        .subcommand(
            commands::context::command_config(),
        )
        .subcommand(
            commands::init::command_config(),
        )
        .subcommand(
            Command::new("server")
                .args_conflicts_with_subcommands(true)
                .flatten_help(true)
                .subcommand(
                    commands::server::command_config(),
                )
        )
        .subcommand(
            commands::apply::command_config(),
        )
        .subcommand(
            commands::login::command_config(),
        )
        .subcommand(
            Command::new("deployment")
                .args_conflicts_with_subcommands(true)
                .flatten_help(true)
                // .args(push_args())
                .subcommand(
                    commands::deployment::list::command_config(),
                )
                .subcommand(
                    commands::deployment::inspect::command_config(),
                )
                .subcommand(
                    commands::deployment::delete::command_config(),
                )
                .subcommand(
                    commands::deployment::logs::command_config(),
                )
        )
        .subcommand(
            Command::new("namespace")
                .args_conflicts_with_subcommands(true)
                .flatten_help(true)
                .subcommand(
                    commands::namespace::prune::command_config(),
                )
        )
        .subcommand(
            Command::new("node")
                .args_conflicts_with_subcommands(true)
                .flatten_help(true)
                .subcommand(
                    commands::node::get::command_config()
                )
        )
        .subcommand(
            Command::new("config")
                .args_conflicts_with_subcommands(true)
                .flatten_help(true)
                .subcommand(
                    commands::config::list::command_config(),
                )
                .subcommand(
                    commands::config::inspect::command_config(),
                )
                .subcommand(
                    commands::config::delete::command_config(),
                )
        )
        .subcommand(
            Command::new("user")
                .args_conflicts_with_subcommands(true)
                .flatten_help(true)
                .subcommand(
                    commands::user::list::command_config(),
                )
                .subcommand(
                    commands::user::create::command_config(),
                )
                .subcommand(
                    commands::user::update::command_config(),
                )
                .subcommand(
                    commands::user::delete::command_config(),
                )
        );

    let matches = app.get_matches();
    let context = matches.get_one::<String>("context")
        .map(|s| s.as_str())
        .unwrap_or("default");

    let subcommand_name = matches.subcommand();
    let config = config::config::load_config(context);

    match subcommand_name {
        Some(("context", sub_matches)) => {
            commands::context::execute(
                sub_matches,
                config,
            );
        }
        Some(("init", sub_matches)) => {
            commands::init::init(sub_matches);
        }
        Some(("server", sub_matches)) => {
            let storage = get_database_connection();
            let server_command = sub_matches.subcommand().unwrap_or(("start", sub_matches));
            match server_command {
                ("start", sub_matches) => {
                    commands::server::execute(
                        sub_matches,
                        config,
                        storage
                    ).await
                }
                _ => {}
            }
        }
        Some(("apply", sub_matches)) => {
          commands::apply::apply(
              sub_matches,
              config,
          );
        }
        Some(("deployment", sub_matches)) => {
            let deployment_command = sub_matches.subcommand().unwrap_or(("list", sub_matches));
            match deployment_command {
                ("list", sub_matches) => {
                    commands::deployment::list::execute(
                        sub_matches,
                        config,
                    );
                }
                ("inspect", sub_matches) => {
                    commands::deployment::inspect::execute(
                        sub_matches,
                        config
                    ).await
                }
                ("delete", sub_matches) => {
                    commands::deployment::delete::execute(
                        sub_matches,
                        config
                    ).await
                }

                ("logs", sub_matches) => {
                    commands::deployment::logs::execute(
                        sub_matches,
                        config
                    ).await
                }
                _ => {}
            }
        }
        Some(("namespace", sub_matches)) => {
            let namespace_command = sub_matches.subcommand().unwrap_or(("prune", sub_matches));
            match namespace_command {
                ("prune", sub_matches) => {
                    commands::namespace::prune::execute(
                        sub_matches,
                        config
                    ).await
                }
                _ => {}
            }
        }
        Some(("node", sub_matches)) => {
            let node_command = sub_matches.subcommand().unwrap_or(("get", sub_matches));
            match node_command {
                ("get", sub_matches) => {
                    commands::node::get::execute(
                        sub_matches,
                        config,   
                    ).await
                }
                _ => {}
            }
        }
        Some(("config", sub_matches)) => {
            let config_command = sub_matches.subcommand().unwrap_or(("list", sub_matches));
            match config_command {
                ("list", sub_matches) => {
                    commands::config::list::execute(
                        sub_matches,
                        config
                    );
                }
                ("inspect", sub_matches) => {
                    commands::config::inspect::execute(
                        sub_matches,
                        config
                    );
                }
                ("delete", sub_matches) => {
                    commands::config::delete::execute(
                        sub_matches,
                        config
                    ).await;
                }
                _ => {}
            }
        }
        Some(("login", sub_matches)) => {
            commands::login::execute(
                sub_matches,
                config,
            );
        }
        Some(("user", sub_matches)) => {
            let user_command = sub_matches.subcommand().unwrap_or(("list", sub_matches));
            match user_command {
                ("list", sub_matches) => {
                    commands::user::list::execute(
                        sub_matches,
                        config
                    );
                }
                ("create", sub_matches) => {
                    commands::user::create::execute(
                        sub_matches,
                        config
                    );
                }
                ("update", sub_matches) => {
                    commands::user::update::execute(
                        sub_matches,
                        config
                    );
                }
                ("delete", sub_matches) => {
                    commands::user::delete::execute(
                        sub_matches,
                        config
                    );
                }
                _ => {}
            }
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

