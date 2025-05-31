use clap::{Command};
use clap::Arg;
use clap::ArgMatches;
use serde_json::json;

use crate::config::config::{Config, load_auth_config};

pub(crate) fn command_config<'a, 'b>() -> Command {
    Command::new("create")
        .about("create user")
        .arg(
            Arg::new("username")
                .short('u')
                .long("username")
                .help("Your username")
                .required(true)
        )
        .arg(
            Arg::new("password")
                .short('p')
                .long("password")
                .help("Your password")
                .required(true)
        )
}

pub(crate) fn execute(args: &ArgMatches, mut configuration: Config) {

    let username = args.get_one::<String>("username");
    let password = args.get_one::<String>("username");

    let auth_config = load_auth_config(configuration.name.clone());

    let api_url = format!("{}/users", configuration.get_api_url());
    let request = ureq::post(&api_url)
        .header("Authorization", &format!("Bearer {}", auth_config.token))
        .send_json(json!({
            "username": username,
            "password": password
        }));

    match request {
        Ok(response) => {
            if response.status() == 201 {
                println!("user creates")
            }
        }
        Err(err) => {
            debug!("{}", err);
            println!("Unable to create user");
        }
    }
}