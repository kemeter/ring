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


pub(crate) async fn execute(args: &ArgMatches, mut configuration: Config) {
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

    let request = reqwest::Client::new()
        .get(&*query)
        .header("Authorization", format!("Bearer {}", auth_config.token))
        .header("Content-Type", "application/json")
        .send()
        .await;

    match request {
        Ok(response) => {
            println!("{:?}", response);
            if response.status() != 200 {
                println!("Unable to fetch configurations list: {}", response.status());
                return;
            }

            let config_list: Vec<ConfigOutput> = response.json::<Vec<ConfigOutput>>().await.unwrap();

            let mut configs = vec![];

            for config in  config_list {
                let data_config: HashMap<String, String> = serde_json::from_str(&config.data).unwrap();

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