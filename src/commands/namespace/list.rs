use crate::config::config::{Config, load_auth_config};
use clap::ArgMatches;
use clap::Command;
use cli_table::{Table, WithTitle, print_stdout};
use serde::Deserialize;

pub(crate) fn command_config() -> Command {
    Command::new("list").about("List namespaces")
}

#[derive(Table)]
struct NamespaceTableItem {
    #[table(title = "Id")]
    id: String,
    #[table(title = "Name")]
    name: String,
    #[table(title = "Created at")]
    created_at: String,
}

#[derive(Deserialize)]
struct NamespaceOutput {
    id: String,
    name: String,
    created_at: String,
}

pub(crate) async fn execute(
    _args: &ArgMatches,
    mut configuration: Config,
    client: &reqwest::Client,
) {
    let api_url = configuration.get_api_url();
    let auth_config = load_auth_config(configuration.name.clone());

    let request = client
        .get(format!("{}/namespaces", api_url))
        .header("Authorization", format!("Bearer {}", auth_config.token))
        .send()
        .await;

    match request {
        Ok(response) => {
            if response.status() != 200 {
                println!("Unable to fetch namespaces: {}", response.status());
                return;
            }

            let namespace_list: Vec<NamespaceOutput> =
                match response.json::<Vec<NamespaceOutput>>().await {
                    Ok(list) => list,
                    Err(e) => {
                        println!("Failed to parse namespace list: {}", e);
                        return;
                    }
                };

            let mut namespaces = vec![];

            for ns in namespace_list {
                namespaces.push(NamespaceTableItem {
                    id: ns.id,
                    name: ns.name,
                    created_at: ns.created_at,
                });
            }

            print_stdout(namespaces.with_title()).expect("");
        }
        Err(error) => {
            println!("Failed to fetch namespaces: {}", error);
        }
    }
}
