use clap::{Command};
use clap::Arg;
use clap::ArgMatches;
use crate::config::config::Config;
use crate::config::config::load_auth_config;

pub(crate) fn command_config<'a, 'b>() -> Command {
    Command::new("delete")
        .about("Delete user")
        .arg(
            Arg::new("id")
        )
}

pub(crate) fn execute(args: &ArgMatches, mut configuration: Config) {
    let api_url = configuration.get_api_url();
    let auth_config = load_auth_config(configuration.name.clone());

    let id = args.get_one::<String>("id").unwrap();

    let request = ureq::delete(&format!("{}/users/{}", api_url, id))
        .set("Authorization", &format!("Bearer {}", auth_config.token))
        .send_json({});

    match request {
        Ok(response) => {
            if response.status() == 204 {
                return println!("User {} deleted ", id);
            }
        }
        Err(err) => {
            debug!("{:?}", err);
            println!("Cannot delete user config");
        }
    }
}