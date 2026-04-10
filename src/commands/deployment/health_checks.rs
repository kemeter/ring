use crate::config::config::Config;
use crate::config::config::load_auth_config;
use crate::exit_code;
use clap::Arg;
use clap::ArgMatches;
use clap::{ArgAction, Command};
use cli_table::{Table, WithTitle, print_stdout};
use serde::Deserialize;

pub(crate) fn command_config() -> Command {
    Command::new("health-checks")
        .about("Show health check results for a deployment")
        .arg(Arg::new("id").help("Deployment ID").required(true))
        .arg(
            Arg::new("latest")
                .long("latest")
                .help("Only return the latest result per check type")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("limit")
                .long("limit")
                .help("Maximum number of results to return")
                .value_parser(clap::value_parser!(u32)),
        )
        .arg(
            Arg::new("output")
                .short('o')
                .long("output")
                .help("Output format")
                .value_parser(["table", "json"])
                .default_value("table"),
        )
}

#[derive(Deserialize, Debug, Clone)]
struct HealthCheckRecord {
    check_type: String,
    status: String,
    message: Option<String>,
    started_at: String,
    finished_at: String,
}

#[derive(Table)]
struct HealthCheckTableItem {
    #[table(title = "Type")]
    check_type: String,
    #[table(title = "Status")]
    status: String,
    #[table(title = "Started")]
    started_at: String,
    #[table(title = "Finished")]
    finished_at: String,
    #[table(title = "Message")]
    message: String,
}

pub(crate) async fn execute(
    args: &ArgMatches,
    mut configuration: Config,
    client: &reqwest::Client,
) {
    let id = args.get_one::<String>("id").unwrap();
    let latest = args.get_flag("latest");
    let limit = args.get_one::<u32>("limit");

    let api_url = configuration.get_api_url();
    let auth_config = load_auth_config(configuration.name.clone());

    let mut url = format!("{}/deployments/{}/health-checks", api_url, id);
    let mut params: Vec<String> = Vec::new();
    if latest {
        params.push("latest=true".to_string());
    }
    if let Some(limit) = limit {
        params.push(format!("limit={}", limit));
    }
    if !params.is_empty() {
        url.push('?');
        url.push_str(&params.join("&"));
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
                eprintln!("Unable to fetch health checks: {}", status);
                exit_code::from_http_status(status.as_u16()).exit();
            }

            let body = match response.text().await {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("Failed to read health check response: {}", e);
                    exit_code::ExitCode::General.exit();
                }
            };

            let output_format = args
                .get_one::<String>("output")
                .map(String::as_str)
                .unwrap_or("table");

            if output_format == "json" {
                println!("{}", body);
                return;
            }

            let results: Vec<HealthCheckRecord> = match serde_json::from_str(&body) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("Failed to parse health checks: {}", e);
                    exit_code::ExitCode::General.exit();
                }
            };

            if results.is_empty() {
                println!("No health check results for deployment {}", id);
                return;
            }

            let rows: Vec<HealthCheckTableItem> = results
                .into_iter()
                .map(|r| HealthCheckTableItem {
                    check_type: r.check_type,
                    status: r.status,
                    started_at: r.started_at,
                    finished_at: r.finished_at,
                    message: r.message.unwrap_or_default(),
                })
                .collect();

            print_stdout(rows.with_title()).expect("");
        }
        Err(error) => {
            eprintln!("Error fetching health checks: {}", error);
            exit_code::from_reqwest_error(&error).exit();
        }
    }
}
