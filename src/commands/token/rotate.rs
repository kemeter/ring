use crate::cli::problem_json::render_response_error;
use crate::config::config::{Config, load_auth_config};
use crate::exit_code;
use clap::Arg;
use clap::ArgMatches;
use clap::Command;
use serde::Deserialize;

pub(crate) fn command_config() -> Command {
    Command::new("rotate")
        .about("Rotate an API token (revokes the old one, mints a new one)")
        .arg(Arg::new("id").required(true).help("Token id to rotate"))
}

#[derive(Deserialize)]
struct TokenRotated {
    token: String,
    token_prefix: String,
}

pub(crate) async fn execute(
    args: &ArgMatches,
    mut configuration: Config,
    client: &reqwest::Client,
) {
    let id = args.get_one::<String>("id").unwrap();

    let api_url = configuration.get_api_url();
    let auth_config = load_auth_config(configuration.name.clone());

    let request = client
        .post(format!("{}/tokens/{}/rotate", api_url, id))
        .header("Authorization", format!("Bearer {}", auth_config.token))
        .send()
        .await;

    match request {
        Ok(response) => {
            let status = response.status();
            if status.is_success() {
                let rotated: TokenRotated = response.json().await.unwrap();
                // Same contract as create: clear value on stdout (capturable),
                // human notice on stderr. The old token is now revoked.
                eprintln!(
                    "Token rotated. The previous token is revoked. Copy the new value now — it will not be shown again."
                );
                eprintln!("  prefix: {}", rotated.token_prefix);
                println!("{}", rotated.token);
            } else {
                let context = format!("Failed to rotate token '{}'", id);
                let code = render_response_error(&context, response).await;
                exit_code::from_http_status(code).exit();
            }
        }
        Err(error) => {
            eprintln!("Failed to rotate token: {}", error);
            exit_code::from_reqwest_error(&error).exit();
        }
    }
}
