use crate::config::config::Config;
use crate::config::config::load_auth_config;
use clap::Arg;
use clap::ArgMatches;
use clap::Command;
use serde_json::json;

use crate::api::dto::user::UserOutput;

pub(crate) fn command_config() -> Command {
    Command::new("update")
        .about("update user")
        .arg(
            Arg::new("username")
                .short('u')
                .long("username")
                .help("Your username")
                .required(false),
        )
        .arg(
            Arg::new("password")
                .short('p')
                .long("password")
                .help("Your password")
                .required(false),
        )
}

pub(crate) async fn execute(
    args: &ArgMatches,
    mut configuration: Config,
    client: &reqwest::Client,
) {
    let auth_config = load_auth_config(configuration.name.clone());

    let user_request = client
        .get(format!("{}/users/me", configuration.get_api_url()))
        .header("Authorization", format!("Bearer {}", auth_config.token))
        .send()
        .await;

    if let Ok(user_response) = user_request {
        let user: UserOutput = match user_response.json::<UserOutput>().await {
            Ok(u) => u,
            Err(e) => {
                println!("Failed to parse user data: {}", e);
                return;
            }
        };

        let api_url = format!("{}/users/{}", configuration.get_api_url(), user.id);

        let binding = user.username;
        let username = args.get_one::<String>("username").unwrap_or(&binding);
        let password = args
            .get_one::<String>("password")
            .cloned()
            .unwrap_or_default();

        let values = if password.is_empty() {
            json!({"username": username})
        } else {
            json!({"username": username, "password": password})
        };

        let request = client
            .put(&api_url)
            .header("Authorization", format!("Bearer {}", auth_config.token))
            .json(&values)
            .send()
            .await;

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
}
