use crate::api::dto::user::UserOutput;
use crate::cli::problem_json::{http_error, render_response_error};
use crate::cli::style;
use crate::config::config::Config;
use crate::config::config::load_auth_config;
use crate::exit_code;
use clap::Arg;
use clap::ArgMatches;
use clap::Command;
use serde_json::json;

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
    let username_arg = args.get_one::<String>("username");
    let password_arg = args.get_one::<String>("password");

    if username_arg.is_none() && password_arg.is_none() {
        eprintln!("Error: at least one of --username or --password must be provided");
        exit_code::ExitCode::General.exit();
    }

    let auth_config = load_auth_config(configuration.name.clone());

    let user_request = client
        .get(format!("{}/users/me", configuration.get_api_url()))
        .header("Authorization", format!("Bearer {}", auth_config.token))
        .send()
        .await;

    let user_response = match user_request {
        Ok(resp) => {
            let status = resp.status();
            if !status.is_success() {
                style::print_error(&http_error(status.as_u16(), "user", "current"));
                exit_code::from_http_status(status.as_u16()).exit();
            }
            resp
        }
        Err(err) => {
            eprintln!("Unable to fetch current user: {}", err);
            exit_code::from_reqwest_error(&err).exit();
        }
    };

    let user: UserOutput = match user_response.json::<UserOutput>().await {
        Ok(u) => u,
        Err(e) => {
            eprintln!("Failed to parse user data: {}", e);
            exit_code::ExitCode::General.exit();
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
            let status = response.status();
            // API returns 200 OK on update (was 201 by accident in the
            // pre-validation code path). Accept any 2xx to stay
            // forward-compatible with future shape changes.
            if status.is_success() {
                println!("user update")
            } else {
                let code = render_response_error("Unable to update user", response).await;
                exit_code::from_http_status(code).exit();
            }
        }
        Err(err) => {
            eprintln!("Unable to update user: {}", err);
            exit_code::from_reqwest_error(&err).exit();
        }
    }
}
