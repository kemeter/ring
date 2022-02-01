use rusqlite::Connection;
use rusqlite::OpenFlags;

#[macro_use]
extern crate log;
extern crate env_logger;
extern crate ureq;

mod commands {
  pub(crate) mod init;
  pub(crate) mod server;
  pub(crate) mod apply;
}
mod scheduler {
  pub(crate) mod scheduler;
}

mod runtime {
  pub(crate) mod docker;
}

mod models {
  pub(crate) mod pods;
}

mod api {
    pub(crate) mod server;
}

use clap::App;

fn main() {
    env_logger::init();

    let commands = vec![
        crate::commands::init::command_config(),
        crate::commands::server::command_config(),
        crate::commands::apply::command_config(),
    ];

    let app = App::new("ring")
      .version("0.0")
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
          crate::commands::server::server(
              matches.subcommand_matches("server:start").unwrap(),
              storage
          )
        }
        Some("apply") => {
          crate::commands::apply::apply(matches.subcommand_matches("apply").unwrap());
        }
        _ => {
          println!("ddd");
        }
    }
}

fn get_database_connection() -> Connection {
    let mut db_flags = OpenFlags::empty();

    db_flags.insert(OpenFlags::SQLITE_OPEN_READ_WRITE);
    db_flags.insert(OpenFlags::SQLITE_OPEN_CREATE);
    db_flags.insert(OpenFlags::SQLITE_OPEN_FULL_MUTEX);
    db_flags.insert(OpenFlags::SQLITE_OPEN_NOFOLLOW);
    db_flags.insert(OpenFlags::SQLITE_OPEN_PRIVATE_CACHE);

    Connection::open_with_flags("ring.db", db_flags).expect("Could not test: DB not created")
}