use crate::api::dto::config::ConfigOutput;
use crate::config::config::{Config, load_auth_config};
use crate::exit_code;
use clap::Arg;
use clap::ArgMatches;
use clap::Command;
use serde_json::Value;
use std::collections::HashMap;

pub(crate) fn command_config() -> Command {
    Command::new("inspect")
        .about("inspect a config map")
        .arg(Arg::new("id").help("Deployment ID").required(true))
        .arg(
            Arg::new("namespace")
                .short('n')
                .long("namespace")
                .help("restrict only namespace"),
        )
}

pub(crate) async fn execute(
    args: &ArgMatches,
    mut configuration: Config,
    client: &reqwest::Client,
) {
    let id = args.get_one::<String>("id").unwrap();
    let api_url = configuration.get_api_url();
    let auth_config = load_auth_config(configuration.name.clone());
    let query = format!("{}/configs/{}", api_url, id);
    let mut params: Vec<String> = Vec::new();

    if args.contains_id("namespace") {
        let namespace = args.get_many::<String>("namespace").unwrap();

        for namespace in namespace {
            params.push(format!("namespace[]={}", namespace));
        }
    }

    let request = client
        .get(&*query)
        .header("Authorization", format!("Bearer {}", auth_config.token))
        .send()
        .await;

    match request {
        Ok(response) => {
            let status = response.status();
            if status != 200 {
                eprintln!("Error: Failed to retrieve configuration details: {}", status);
                exit_code::from_http_status(status.as_u16()).exit();
            }

            let config: ConfigOutput = match response.json::<ConfigOutput>().await {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("Failed to parse config: {}", e);
                    exit_code::ExitCode::General.exit();
                }
            };

            let data: Value =
                serde_json::from_str(&config.data).expect("Failed to parse config data as JSON");

            let data_config: HashMap<String, String> = data
                .as_object()
                .unwrap_or(&serde_json::Map::new())
                .iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect();

            // Affichage au style kubectl
            println!("-------");
            println!();
            println!("Name:         {}", config.name);
            println!("Namespace:    {}", config.namespace);
            println!("Labels:       {}", config.labels);
            println!();
            println!("Data");
            println!("====");

            // Afficher les données de configuration
            for (key, value) in data_config {
                println!("{}:", key);
                println!("----");
                println!("{}", value);
                println!();
            }
        }
        Err(err) => {
            eprintln!("Error: Failed to retrieve configuration details: {}", err);
            exit_code::from_reqwest_error(&err).exit();
        }
    }
}
