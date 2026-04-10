use clap::Arg;
use clap::ArgMatches;
use clap::Command;
use serde_json::json;

use crate::config::config::{Config, load_auth_config};
use crate::exit_code;

pub(crate) fn command_config() -> Command {
    Command::new("create")
        .about("create user")
        .arg(
            Arg::new("username")
                .short('u')
                .long("username")
                .help("Your username")
                .required(true),
        )
        .arg(
            Arg::new("password")
                .short('p')
                .long("password")
                .help("Your password")
                .required(true),
        )
}

pub(crate) async fn execute(
    args: &ArgMatches,
    mut configuration: Config,
    client: &reqwest::Client,
) {
    let username = args.get_one::<String>("username");
    let password = args.get_one::<String>("password");

    let auth_config = load_auth_config(configuration.name.clone());

    let api_url = format!("{}/users", configuration.get_api_url());
    let request = client
        .post(&api_url)
        .header("Authorization", format!("Bearer {}", auth_config.token))
        .json(&json!({
            "username": username,
            "password": password
        }))
        .send()
        .await;

    match request {
        Ok(response) => {
            let status = response.status();
            if status == 201 {
                println!("user creates")
            } else {
                eprintln!("Unable to create user: {}", status);
                exit_code::from_http_status(status.as_u16()).exit();
            }
        }
        Err(err) => {
            debug!("{}", err);
            eprintln!("Unable to create user: {}", err);
            exit_code::from_reqwest_error(&err).exit();
        }
    }
}
