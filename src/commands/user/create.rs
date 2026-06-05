use clap::Arg;
use clap::ArgMatches;
use clap::Command;
use serde_json::json;

use crate::cli::problem_json::render_response_error;
use crate::config::auth::load_auth_config;
use crate::config::config::Config;
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
                let code = render_response_error("Unable to create user", response).await;
                exit_code::from_http_status(code).exit();
            }
        }
        Err(err) => {
            debug!("{}", err);
            eprintln!("Unable to create user: {}", err);
            exit_code::from_reqwest_error(&err).exit();
        }
    }
}
