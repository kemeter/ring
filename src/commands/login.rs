use crate::config::config::Config;
use crate::exit_code;
use clap::Arg;
use clap::ArgMatches;
use clap::Command;
use serde_json::json;
use std::collections::HashMap;
use std::fs;

use crate::config::config::AuthToken;
use crate::config::config::get_config_dir;
use std::string::String;

pub(crate) fn command_config() -> Command {
    Command::new("login")
        .about("Login to your account")
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
    let username = args.get_one::<String>("username").unwrap();
    let password = args.get_one::<String>("password").unwrap();

    let config_directory = get_config_dir();
    let config_file = format!("{}/auth.json", config_directory);

    let api_url = format!("{}/login", configuration.get_api_url());
    let request = client
        .post(&api_url)
        .json(&json!({
            "username": username,
            "password": password
        }))
        .send()
        .await;

    match request {
        Ok(response) => {
            let status = response.status();
            if status != 200 {
                eprintln!("Login failed: {}", status);
                exit_code::from_http_status(status.as_u16()).exit();
            }

            let auth = match response.json::<AuthToken>().await {
                Ok(a) => a,
                Err(e) => {
                    eprintln!("Failed to parse authentication response: {}", e);
                    exit_code::ExitCode::General.exit();
                }
            };

            let auth_file_content =
                fs::read_to_string(config_file.clone()).unwrap_or_else(|_| "{}".to_string());

            let mut context_auth: HashMap<String, AuthToken> =
                serde_json::from_str(&auth_file_content).unwrap_or_default();

            context_auth.insert(configuration.name, auth);

            let serialized_data = match serde_json::to_string(&context_auth) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("Failed to serialize auth data: {}", e);
                    exit_code::ExitCode::General.exit();
                }
            };

            if let Err(e) = fs::create_dir_all(&config_directory) {
                eprintln!("Failed to create config directory: {}", e);
                exit_code::ExitCode::General.exit();
            }

            if let Err(e) = fs::write(config_file, serialized_data) {
                eprintln!("Failed to write auth file: {}", e);
                exit_code::ExitCode::General.exit();
            }
            println!("Logging in as {}", username);
        }
        Err(err) => {
            eprintln!("Connection failed: {}", err);
            exit_code::from_reqwest_error(&err).exit();
        }
    }
}
