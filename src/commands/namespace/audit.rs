use crate::config::auth::load_auth_config;
use crate::config::config::Config;
use crate::exit_code;
use clap::Arg;
use clap::ArgMatches;
use clap::Command;
use cli_table::{Table, WithTitle, print_stdout};
use serde::Deserialize;

pub(crate) fn command_config() -> Command {
    Command::new("audit")
        .about("Show the write-action audit trail for a namespace")
        .arg(Arg::new("namespace").required(true).help("Namespace name"))
        .arg(
            Arg::new("limit")
                .long("limit")
                .help("Maximum number of entries (most recent first)"),
        )
}

#[derive(Table)]
struct AuditTableItem {
    #[table(title = "Timestamp")]
    timestamp: String,
    #[table(title = "User")]
    user_id: String,
    #[table(title = "Action")]
    action: String,
    #[table(title = "Type")]
    target_type: String,
    #[table(title = "Target")]
    target_name: String,
}

#[derive(Deserialize)]
struct AuditOutput {
    timestamp: String,
    user_id: Option<String>,
    action: String,
    target_type: String,
    target_name: String,
}

pub(crate) async fn execute(
    args: &ArgMatches,
    mut configuration: Config,
    client: &reqwest::Client,
) {
    let namespace = args
        .get_one::<String>("namespace")
        .expect("namespace is required");
    let api_url = configuration.get_api_url();
    let auth_config = load_auth_config(configuration.name.clone());

    let mut url = format!("{}/namespaces/{}/audit", api_url, namespace);
    if let Some(limit) = args.get_one::<String>("limit") {
        url.push_str(&format!("?limit={}", limit));
    }

    let request = client
        .get(&url)
        .header("Authorization", format!("Bearer {}", auth_config.token))
        .send()
        .await;

    match request {
        Ok(response) => {
            let status = response.status();
            if status != 200 {
                eprintln!("Unable to fetch audit log: {}", status);
                exit_code::from_http_status(status.as_u16()).exit();
            }

            let entries: Vec<AuditOutput> = match response.json::<Vec<AuditOutput>>().await {
                Ok(list) => list,
                Err(e) => {
                    eprintln!("Failed to parse audit log: {}", e);
                    exit_code::ExitCode::General.exit();
                }
            };

            if entries.is_empty() {
                println!("No audit entries for namespace '{}'", namespace);
                return;
            }

            let rows: Vec<AuditTableItem> = entries
                .into_iter()
                .map(|e| AuditTableItem {
                    timestamp: e.timestamp,
                    user_id: e.user_id.unwrap_or_else(|| "-".to_string()),
                    action: e.action,
                    target_type: e.target_type,
                    target_name: e.target_name,
                })
                .collect();

            print_stdout(rows.with_title()).expect("");
        }
        Err(error) => {
            eprintln!("Failed to fetch audit log: {}", error);
            exit_code::from_reqwest_error(&error).exit();
        }
    }
}
