use crate::config::config::{Config, load_auth_config};
use clap::Arg;
use clap::ArgMatches;
use clap::Command;
use serde::{Deserialize, Serialize};

pub(crate) fn command_config() -> Command {
    Command::new("create")
        .about("Create a namespace")
        .arg(Arg::new("name").required(true).help("Namespace name"))
}

#[derive(Serialize)]
struct NamespaceInput {
    name: String,
}

#[derive(Deserialize)]
struct NamespaceOutput {
    id: String,
    name: String,
}

#[derive(Deserialize)]
struct ErrorResponse {
    error: String,
}

pub(crate) async fn execute(
    args: &ArgMatches,
    mut configuration: Config,
    client: &reqwest::Client,
) {
    let name = args.get_one::<String>("name").unwrap();

    let api_url = configuration.get_api_url();
    let auth_config = load_auth_config(configuration.name.clone());

    let input = NamespaceInput { name: name.clone() };

    let request = client
        .post(format!("{}/namespaces", api_url))
        .header("Authorization", format!("Bearer {}", auth_config.token))
        .json(&input)
        .send()
        .await;

    match request {
        Ok(response) => {
            let status = response.status();
            if status == 201 {
                let namespace: NamespaceOutput = response.json().await.unwrap();
                println!(
                    "Namespace '{}' created (id: {})",
                    namespace.name, namespace.id
                );
            } else if status == 409 {
                let error: ErrorResponse = response.json().await.unwrap();
                println!("Error: {}", error.error);
            } else {
                println!("Failed to create namespace: {}", status);
            }
        }
        Err(error) => {
            println!("Failed to create namespace: {}", error);
        }
    }
}
