use crate::cli::problem_json::render_response_error;
use crate::cli::style;
use crate::config::auth::load_auth_config;
use crate::config::config::Config;
use crate::exit_code;
use clap::Arg;
use clap::ArgAction;
use clap::ArgMatches;
use clap::Command;
use serde::Deserialize;

pub(crate) fn command_config() -> Command {
    Command::new("update")
        .about("Replace the scopes of an API token, keeping its value")
        .arg(Arg::new("id").required(true).help("Token id"))
        .arg(
            Arg::new("scope")
                .short('s')
                .long("scope")
                .action(ArgAction::Append)
                .required(true)
                .help("Scope (verb:resource), repeatable. The token gets exactly these scopes"),
        )
}

#[derive(Deserialize)]
struct UpdatedToken {
    name: String,
    scopes: Vec<String>,
}

pub(crate) async fn execute(
    args: &ArgMatches,
    mut configuration: Config,
    client: &reqwest::Client,
) {
    let id = args.get_one::<String>("id").unwrap();
    let scopes: Vec<String> = args
        .get_many::<String>("scope")
        .map(|values| values.cloned().collect())
        .unwrap_or_default();

    let api_url = configuration.get_api_url();
    let auth_config = load_auth_config(configuration.name.clone());

    let request = client
        .patch(format!("{}/tokens/{}", api_url, id))
        .header("Authorization", format!("Bearer {}", auth_config.token))
        .json(&serde_json::json!({ "scopes": scopes }))
        .send()
        .await;

    match request {
        Ok(response) => {
            let status = response.status();
            if status.is_success() {
                match response.json::<UpdatedToken>().await {
                    Ok(token) => style::print_success(&format!(
                        "Token '{}' now has scopes: {}",
                        token.name,
                        token.scopes.join(", ")
                    )),
                    Err(_) => style::print_success(&format!("Token '{}' updated", id)),
                }
            } else {
                let context = format!("Failed to update token '{}'", id);
                let code = render_response_error(&context, response).await;
                exit_code::from_http_status(code).exit();
            }
        }
        Err(error) => {
            eprintln!("Failed to update token: {}", error);
            exit_code::from_reqwest_error(&error).exit();
        }
    }
}
