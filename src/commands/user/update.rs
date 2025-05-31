use clap::{Command};
use clap::Arg;
use clap::ArgMatches;
use serde_json::json;
use crate::config::config::Config;
use crate::config::config::load_auth_config;

use crate::api::dto::user::UserOutput;

pub(crate) fn command_config<'a, 'b>() -> Command {
    Command::new("update")
        .about("update user")
        .arg(
            Arg::new("username")
                .short('u')
                .long("username")
                .help("Your username")
                .required(false)
        )
        .arg(
            Arg::new("password")
                .short('p')
                .long("password")
                .help("Your password")
                .required(false)
        )
}

pub(crate) fn execute(args: &ArgMatches, mut configuration: Config) {
    let auth_config = load_auth_config(configuration.name.clone());

    let user_request = ureq::get(&format!("{}/users/me", configuration.get_api_url()))
        .header("Authorization", &format!("Bearer {}", auth_config.token))
        .call();

    match user_request {
        Ok(mut user_response) => {
            let user: UserOutput = user_response.body_mut().read_json::<UserOutput>().unwrap();

            let api_url = format!("{}/users/{}", configuration.get_api_url(), user.id);

            let binding = String::from(user.username);
            let username = args.get_one::<String>("username").unwrap_or(&binding);
            let password = args.get_one::<String>("password").unwrap();

            let values = if password.is_empty() {
                json!({"username": username})
            } else {
                json!({"username": username, "password": password})
            };

            let request = ureq::put(&api_url)
                 .header("Authorization", &format!("Bearer {}", auth_config.token))
                 .send_json(values);

            match request {
                Ok(response) => {
                    if response.status() == 201 {
                        println!("user update")
                    }
                }
                Err(_) => {
                    println!("Unable to update user");
                }
            }
        }
        _ => {}
    }
}
