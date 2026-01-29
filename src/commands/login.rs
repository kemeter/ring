use std::collections::HashMap;
use clap::{Command};
use clap::Arg;
use clap::ArgMatches;
use std::fs;
use serde_json::json;
use crate::config::config::Config;

use crate::config::config::AuthToken;
use crate::config::config::get_config_dir;
use std::string::String;

pub(crate) fn command_config<'a, 'b>() -> Command {
    Command::new("login")
        .about("Login to your account")
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

pub(crate) async fn execute(args: &ArgMatches, mut configuration: Config, client: &reqwest::Client) {
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
            if response.status() == 200 {
                let auth = match response.json::<AuthToken>().await {
                    Ok(a) => a,
                    Err(e) => {
                        println!("Failed to parse authentication response: {}", e);
                        return;
                    }
                };

                let auth_file_content = fs::read_to_string(config_file.clone()).unwrap_or_else(|_| "{}".to_string());

                let mut context_auth: HashMap<String, AuthToken> = serde_json::from_str(&auth_file_content).unwrap_or_default();

                context_auth.insert(configuration.name, auth);

                let serialized_data = match serde_json::to_string(&context_auth) {
                    Ok(s) => s,
                    Err(e) => {
                        println!("Failed to serialize auth data: {}", e);
                        return;
                    }
                };

                if let Err(e) = fs::create_dir_all(&config_directory) {
                    println!("Failed to create config directory: {}", e);
                    return;
                }

                if let Err(e) = fs::write(config_file, serialized_data) {
                    println!("Failed to write auth file: {}", e);
                    return;
                }
                return println!("Logging in as {}", username);
            }
        }
        Err(_err) => {
            println!("Wrong credentials");
        }
    }
}
