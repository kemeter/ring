use std::collections::HashMap;
use clap::App;
use clap::Arg;
use clap::SubCommand;
use clap::ArgMatches;
use std::fs;
use ureq::json;
use crate::config::config::Config;
use crate::config::config::get_config_dir;
use std::string::String;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
struct AuthToken {
    token: String,
}

pub(crate) fn command_config<'a, 'b>() -> App<'a, 'b> {
    SubCommand::with_name("login")
        .about("Login to your account")
        .arg(
            Arg::with_name("username")
                .short("u")
                .long("username")
                .help("Your username")
                .takes_value(true)
                .required(true)
        )
        .arg(
            Arg::with_name("password")
                .short("p")
                .long("password")
                .help("Your password")
                .takes_value(true)
                .required(true)
        )
}

pub(crate) fn execute(args: &ArgMatches, mut configuration: Config) {
    let username = args.value_of("username").unwrap();
    let password = args.value_of("password").unwrap();

    let config_directory = get_config_dir();
    let config_file = format!("{}/auth.json", config_directory);

    let api_url = format!("{}/login", configuration.get_api_url());
    let request = ureq::post(&api_url)
        .send_json(json!({
            "username": username,
            "password": password
        }));

    match request {
        Ok(response) => {
            if response.status() == 200 {
                let content = response.into_string().unwrap();
                let auth: AuthToken = serde_json::from_str(&content).unwrap();

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
            println!("Unable to login");
        }
    }
}
