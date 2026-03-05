use crate::config::config::{load_auth_config, Config};
use clap::Arg;
use clap::ArgMatches;
use clap::Command;
use cli_table::{print_stdout, Table, WithTitle};
use serde::Deserialize;

pub(crate) fn command_config() -> Command {
    Command::new("list")
        .about("List secrets")
        .arg(
            Arg::new("namespace")
                .short('n')
                .long("namespace")
                .help("Filter by namespace")
        )
}

#[derive(Deserialize)]
struct SecretOutput {
    id: String,
    name: String,
    namespace: String,
    created_at: String,
    updated_at: Option<String>,
}

#[derive(Table)]
struct SecretTableItem {
    #[table(title = "Id")]
    id: String,
    #[table(title = "Name")]
    name: String,
    #[table(title = "Namespace")]
    namespace: String,
    #[table(title = "Created at")]
    created_at: String,
    #[table(title = "Updated at")]
    updated_at: String,
}

pub(crate) async fn execute(args: &ArgMatches, mut configuration: Config, client: &reqwest::Client) {
    let api_url = configuration.get_api_url();
    let auth_config = load_auth_config(configuration.name.clone());
    let mut params: Vec<String> = Vec::new();

    if args.contains_id("namespace") {
        if let Some(namespaces) = args.get_many::<String>("namespace") {
            for namespace in namespaces {
                params.push(format!("namespace[]={}", namespace));
            }
        }
    }

    let query = if !params.is_empty() {
        format!("{}/secrets?{}", api_url, params.join("&"))
    } else {
        format!("{}/secrets", api_url)
    };

    let request = client
        .get(&query)
        .header("Authorization", format!("Bearer {}", auth_config.token))
        .send()
        .await;

    match request {
        Ok(response) => {
            if response.status() != 200 {
                println!("Unable to fetch secrets list: {}", response.status());
                return;
            }

            let secret_list: Vec<SecretOutput> = match response.json().await {
                Ok(list) => list,
                Err(e) => {
                    println!("Failed to parse secret list: {}", e);
                    return;
                }
            };

            let secrets: Vec<SecretTableItem> = secret_list
                .into_iter()
                .map(|s| SecretTableItem {
                    id: s.id,
                    name: s.name,
                    namespace: s.namespace,
                    created_at: s.created_at,
                    updated_at: s.updated_at.unwrap_or_default(),
                })
                .collect();

            print_stdout(secrets.with_title()).expect("Failed to print table");
        }
        Err(error) => {
            println!("Failed to fetch secrets: {}", error);
        }
    }
}
