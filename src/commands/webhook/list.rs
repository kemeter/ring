use crate::cli::problem_json::http_error_global_list;
use crate::cli::style;
use crate::config::auth::load_auth_config;
use crate::config::config::Config;
use crate::exit_code;
use clap::ArgMatches;
use clap::Command;
use cli_table::{Table, WithTitle};
use serde::Deserialize;

pub(crate) fn command_config() -> Command {
    Command::new("list").about("List webhook subscribers")
}

#[derive(Deserialize)]
struct WebhookOutput {
    id: String,
    url: String,
    events: Vec<String>,
    created_at: String,
    revoked_at: Option<String>,
}

#[derive(Table)]
struct WebhookTableItem {
    #[table(title = "Id")]
    id: String,
    #[table(title = "URL")]
    url: String,
    #[table(title = "Events")]
    events: String,
    #[table(title = "Status")]
    status: String,
    #[table(title = "Created (UTC)")]
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
        .get(format!("{}/webhooks", api_url))
        .header("Authorization", format!("Bearer {}", auth_config.token))
        .send()
        .await;

    match request {
        Ok(response) => {
            let status = response.status();
            if status != 200 {
                style::print_error(&http_error_global_list(status.as_u16(), "webhooks"));
                exit_code::from_http_status(status.as_u16()).exit();
            }

            let hooks: Vec<WebhookOutput> = match response.json().await {
                Ok(list) => list,
                Err(e) => {
                    eprintln!("Failed to parse webhook list: {}", e);
                    exit_code::ExitCode::General.exit();
                }
            };

            let items: Vec<WebhookTableItem> = hooks
                .into_iter()
                .map(|w| WebhookTableItem {
                    id: w.id,
                    url: w.url,
                    events: if w.events.is_empty() {
                        "all".to_string()
                    } else {
                        w.events.join(",")
                    },
                    status: if w.revoked_at.is_some() {
                        style::status_custom("revoked", style::StatusColour::Red)
                    } else {
                        style::status_custom("active", style::StatusColour::Green)
                    },
                    created_at: style::format_date(&w.created_at),
                })
                .collect();

            style::print_table(items.with_title());
        }
        Err(error) => {
            eprintln!("Failed to fetch webhooks: {}", error);
            exit_code::from_reqwest_error(&error).exit();
        }
    }
}
