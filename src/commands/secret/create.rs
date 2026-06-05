use crate::cli::problem_json::render_response_error;
use crate::config::auth::load_auth_config;
use crate::config::config::Config;
use crate::exit_code;
use clap::Arg;
use clap::ArgMatches;
use clap::Command;
use serde::{Deserialize, Serialize};

pub(crate) fn command_config() -> Command {
    Command::new("create")
        .about("Create a secret")
        .arg(Arg::new("name").required(true).help("Secret name"))
        .arg(
            Arg::new("namespace")
                .short('n')
                .long("namespace")
                .required(true)
                .help("Namespace"),
        )
        .arg(
            Arg::new("value")
                .short('v')
                .long("value")
                .required(true)
                .help("Secret value"),
        )
}

#[derive(Serialize)]
struct SecretInput {
    name: String,
    namespace: String,
    value: String,
}

#[derive(Deserialize)]
struct SecretOutput {
    id: String,
    name: String,
    namespace: String,
}

pub(crate) async fn execute(
    args: &ArgMatches,
    mut configuration: Config,
    client: &reqwest::Client,
) {
    let name = args.get_one::<String>("name").unwrap();
    let namespace = args.get_one::<String>("namespace").unwrap();
    let value = args.get_one::<String>("value").unwrap();

    let api_url = configuration.get_api_url();
    let auth_config = load_auth_config(configuration.name.clone());

    let input = SecretInput {
        name: name.clone(),
        namespace: namespace.clone(),
        value: value.clone(),
    };

    let request = client
        .post(format!("{}/secrets", api_url))
        .header("Authorization", format!("Bearer {}", auth_config.token))
        .json(&input)
        .send()
        .await;

    match request {
        Ok(response) => {
            let status = response.status();
            if status.is_success() {
                let secret: SecretOutput = response.json().await.unwrap();
                println!(
                    "Secret '{}' created in namespace '{}' (id: {})",
                    secret.name, secret.namespace, secret.id
                );
            } else {
                let context = format!("Failed to create secret '{}'", name);
                let code = render_response_error(&context, response).await;
                exit_code::from_http_status(code).exit();
            }
        }
        Err(error) => {
            eprintln!("Failed to create secret: {}", error);
            exit_code::from_reqwest_error(&error).exit();
        }
    }
}
