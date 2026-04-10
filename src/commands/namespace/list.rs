use crate::config::config::{Config, load_auth_config};
use crate::exit_code;
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
            let status = response.status();
            if status != 200 {
                eprintln!("Unable to fetch namespaces: {}", status);
                exit_code::from_http_status(status.as_u16()).exit();
            }

            let namespace_list: Vec<NamespaceOutput> =
                match response.json::<Vec<NamespaceOutput>>().await {
                    Ok(list) => list,
                    Err(e) => {
                        eprintln!("Failed to parse namespace list: {}", e);
                        exit_code::ExitCode::General.exit();
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
            eprintln!("Failed to fetch namespaces: {}", error);
            exit_code::from_reqwest_error(&error).exit();
        }
    }
}
