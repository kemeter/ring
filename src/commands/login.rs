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
                let auth: AuthToken = response.json().await.unwrap();

                let auth_file_content = fs::read_to_string(config_file.clone()).unwrap();

                let mut context_auth: HashMap<String, AuthToken> = serde_json::from_str(&auth_file_content).unwrap();

                context_auth.insert(configuration.name, auth);

                let serialized_data = serde_json::to_string(&context_auth).unwrap();

                fs::create_dir_all(&config_directory).unwrap();
                fs::write(config_file, serialized_data).expect("Unable to write file");
                return println!("Logging in as {}", username);
            }
        }
        Err(_err) => {
            println!("Wrong credentials");
        }
    }
}
