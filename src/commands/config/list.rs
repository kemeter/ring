use std::collections::HashMap;
use crate::api::dto::config::ConfigOutput;
use crate::config::config::{load_auth_config, Config};
use clap::Arg;
use clap::ArgMatches;
use clap::Command;
use cli_table::{print_stdout, Table, WithTitle};

pub(crate) fn command_config<'a, 'b>() -> Command {
    Command::new("list")
        .about("List of config maps")
        .arg(
            Arg::new("namespace")
                .short('n')
                .long("namespace")
                .help("restrict only namespace")
        )
}

#[derive(Table)]
struct ConfigTableItem {
    #[table(title = "Id")]
    id: String,
    #[table(title = "Name")]
    name: String,
    #[table(title = "Created at")]
    created_at: String,
    #[table(title = "Updated at")]
    updated_at: String,
    #[table(title = "Namespace")]
    namespace: String,
    #[table(title = "Data")]
    data: f64,
}


pub(crate) async fn execute(args: &ArgMatches, mut configuration: Config, client: &reqwest::Client) {
    let api_url = configuration.get_api_url();
    let auth_config = load_auth_config(configuration.name.clone());
    let query = format!("{}/configs", api_url);
    let mut params: Vec<String> = Vec::new();

    if args.contains_id("namespace"){
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
            println!("{:?}", response);
            if response.status() != 200 {
                println!("Unable to fetch configurations list: {}", response.status());
                return;
            }

            let config_list: Vec<ConfigOutput> = match response.json::<Vec<ConfigOutput>>().await {
                Ok(list) => list,
                Err(e) => {
                    println!("Failed to parse config list: {}", e);
                    return;
                }
            };

            let mut configs = vec![];

            for config in  config_list {
                let data_config: HashMap<String, String> = match serde_json::from_str(&config.data) {
                    Ok(data) => data,
                    Err(e) => {
                        eprintln!("Failed to parse config data for {}: {}", config.name, e);
                        HashMap::new()
                    }
                };

                configs.push(ConfigTableItem {
                    id: config.id.to_string(),
                    created_at: config.created_at.to_string(),
                    updated_at: config.updated_at.unwrap_or_default(),
                    name: config.name,
                    namespace: config.namespace,
                    data: data_config.len() as f64,
                });
            }

            print_stdout(configs.with_title()).expect("");
        }
        Err(error) => {
            println!("Failed to fetch configurations: {}", error);
        }

    }
}