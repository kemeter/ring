use clap::App;
use clap::Arg;
use clap::SubCommand;
use clap::ArgMatches;
use ureq::json;
use serde_json::Result;
use crate::config::config::Config;
use crate::config::config::load_auth_config;
use serde::{Serialize, Deserialize};
use crate::api::dto::user::UserOutput;

pub(crate) fn command_config<'a, 'b>() -> App<'a, 'b> {
    SubCommand::with_name("user:update")
        .about("create user")
        .arg(
            Arg::with_name("username")
                .short("u")
                .long("username")
                .takes_value(true)
                .help("Your username")
                .required(false)
        )
        .arg(
            Arg::with_name("password")
                .short("p")
                .long("password")
                .takes_value(true)
                .help("Your password")
                .required(false)
        )
}

pub(crate) fn execute(args: &ArgMatches, mut configuration: Config) {
    let auth_config = load_auth_config();

    let user_request = ureq::get(&format!("{}/users/me", configuration.get_api_url()))
        .set("Authorization", &format!("Bearer {}", auth_config.token))
        .send_json({});

    match user_request {
        Ok(user_response ) => {
            let response_content = user_response.into_string().unwrap();
            let value: Result<UserOutput> = serde_json::from_str(&response_content);
            let user = value.unwrap();

            let api_url = format!("{}/users/{}", configuration.get_api_url(), user.id);

            let username = args.value_of("username").unwrap_or(&*user.username);
            let password = args.value_of("password").unwrap();

            let values = if password.is_empty() {
                json!({"username": username})
            } else {
                json!({"username": username, "password": password})
            };

            let request = ureq::put(&api_url)
                 .set("Authorization", &format!("Bearer {}", auth_config.token))
                 .send_json(values);

            match request {
                Ok(response) => {
                    if response.status() == 201 {
                        println!("user update")
                    }
                }
                Err(err) => {
                    println!("Unable to update user");
                }
            }
        }
        _ => {}
    }
}
