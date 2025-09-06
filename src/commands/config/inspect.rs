use std::collections::HashMap;
use crate::config::config::{load_auth_config, Config};
use clap::Arg;
use clap::ArgMatches;
use clap::Command;
use serde_json::Value;
use crate::api::dto::config::ConfigOutput;

pub(crate) fn command_config<'a, 'b>() -> Command {
    Command::new("inspect")
        .about("inspect a config map")
        .arg(
            Arg::new("id")
                .help("Deployment ID")
                .required(true)
        )
        .arg(
            Arg::new("namespace")
                .short('n')
                .long("namespace")
                .help("restrict only namespace")
        )
}

pub(crate) async fn execute(args: &ArgMatches, mut configuration: Config) {
    let id = args.get_one::<String>("id").unwrap();
    let api_url = configuration.get_api_url();
    let auth_config = load_auth_config(configuration.name.clone());
    let query = format!("{}/configs/{}", api_url, id);
    let mut params: Vec<String> = Vec::new();

    if args.contains_id("namespace"){
        let namespace = args.get_many::<String>("namespace").unwrap();

        for namespace in namespace {
            params.push(format!("namespace[]={}", namespace));
        }
    }

    let request = reqwest::Client::new()
        .get(&*query)
        .header("Authorization", format!("Bearer {}", auth_config.token))
        .header("Content-Type", "application/json")
        .send()
        .await;

    match request {
        Ok(response) => {
            if response.status() != 200 {
                println!("Error: Failed to retrieve configuration details");
                return;
            }

            let config: ConfigOutput = response.json::<ConfigOutput>().await.unwrap();

            let data: Value = serde_json::from_str(&config.data)
                .expect("Failed to parse config data as JSON");

            let data_config: HashMap<String, String> = data
                .as_object()
                .unwrap_or(&serde_json::Map::new())
                .iter()
                .filter_map(|(k, v)| {
                    v.as_str().map(|s| (k.clone(), s.to_string()))
                })
                .collect();

            // Affichage au style kubectl
            println!("-------");
            println!("");
            println!("Name:         {}", config.name);
            println!("Namespace:    {}", config.namespace);
            println!("Labels:       {}", config.labels);
            println!("");
            println!("Data");
            println!("====");

            // Afficher les données de configuration
            for (key, value) in data_config {
                println!("{}:", key);
                println!("----");
                println!("{}", value);
                println!("");
            }
        }
        Err(_) => {
            println!("Error: Failed to retrieve configuration details");
        }
    }

}