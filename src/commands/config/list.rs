use crate::api::dto::config::ConfigOutput;
use crate::cli::problem_json::http_error_list;
use crate::cli::style;
use crate::config::config::{Config, load_auth_config};
use crate::exit_code;
use clap::Arg;
use clap::ArgMatches;
use clap::Command;
use cli_table::{Table, WithTitle};
use std::collections::HashMap;

pub(crate) fn command_config() -> Command {
    Command::new("list").about("List of config maps").arg(
        Arg::new("namespace")
            .short('n')
            .long("namespace")
            .help("restrict only namespace"),
    )
}

#[derive(Table)]
struct ConfigTableItem {
    #[table(title = "Id")]
    id: String,
    #[table(title = "Name")]
    name: String,
    #[table(title = "Created at (UTC)")]
    created_at: String,
    #[table(title = "Updated at (UTC)")]
    updated_at: String,
    #[table(title = "Namespace")]
    namespace: String,
    #[table(title = "Data")]
    data: usize,
}

pub(crate) async fn execute(
    args: &ArgMatches,
    mut configuration: Config,
    client: &reqwest::Client,
) {
    let api_url = configuration.get_api_url();
    let auth_config = load_auth_config(configuration.name.clone());
    let query = format!("{}/configs", api_url);
    let mut params: Vec<String> = Vec::new();

    let mut ns_scope: Vec<String> = Vec::new();
    if args.contains_id("namespace") {
        let namespace = args.get_many::<String>("namespace").unwrap();

        for namespace in namespace {
            params.push(format!("namespace[]={}", namespace));
            ns_scope.push(namespace.clone());
        }
    }
    let ns_label = if ns_scope.is_empty() {
        "all".to_string()
    } else {
        ns_scope.join(",")
    };

    let query = if !params.is_empty() {
        format!("{}?{}", query, params.join("&"))
    } else {
        query
    };

    let request = client
        .get(&*query)
        .header("Authorization", format!("Bearer {}", auth_config.token))
        .send()
        .await;

    match request {
        Ok(response) => {
            let status = response.status();
            if status != 200 {
                style::print_error(&http_error_list(status.as_u16(), "configs", &ns_label));
                exit_code::from_http_status(status.as_u16()).exit();
            }

            let config_list: Vec<ConfigOutput> = match response.json::<Vec<ConfigOutput>>().await {
                Ok(list) => list,
                Err(e) => {
                    eprintln!("Failed to parse config list: {}", e);
                    exit_code::ExitCode::General.exit();
                }
            };

            let mut configs = vec![];

            for config in config_list {
                let data_config: HashMap<String, String> = match serde_json::from_str(&config.data)
                {
                    Ok(data) => data,
                    Err(e) => {
                        eprintln!("Failed to parse config data for {}: {}", config.name, e);
                        HashMap::new()
                    }
                };

                configs.push(ConfigTableItem {
                    id: config.id.to_string(),
                    created_at: style::format_date(&config.created_at.to_string()),
                    updated_at: style::format_date(&config.updated_at.unwrap_or_default()),
                    name: config.name,
                    namespace: config.namespace,
                    data: data_config.len(),
                });
            }

            style::print_table(configs.with_title());
        }
        Err(error) => {
            eprintln!("Failed to fetch configurations: {}", error);
            exit_code::from_reqwest_error(&error).exit();
        }
    }
}
