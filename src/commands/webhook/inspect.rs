use crate::cli::problem_json::http_error;
use crate::cli::style;
use crate::config::config::{Config, load_auth_config};
use crate::exit_code;
use clap::{Arg, ArgMatches, Command};
use cli_table::{Table, WithTitle};
use serde::Deserialize;
use std::time::Duration;

/// Polling interval for `--follow`. Short enough to feel live in a terminal,
/// long enough that a slow SQLite tick or a hung subscriber doesn't get
/// hammered. The events table is local on the Ring host, so the cost is small.
const FOLLOW_INTERVAL: Duration = Duration::from_secs(2);

pub(crate) fn command_config() -> Command {
    Command::new("inspect")
        .about("Show recent events offered to a webhook")
        .arg(Arg::new("id").help("Webhook id").required(true).index(1))
        .arg(
            Arg::new("follow")
                .long("follow")
                .short('f')
                .help("Keep polling for new events")
                .num_args(0),
        )
}

#[derive(Deserialize, Clone)]
struct EventOutput {
    id: String,
    kind: String,
    status: String,
    attempts: i64,
    created_at: String,
    updated_at: Option<String>,
    last_error: Option<String>,
}

#[derive(Table)]
struct EventTableItem {
    #[table(title = "Id")]
    id: String,
    #[table(title = "Kind")]
    kind: String,
    #[table(title = "Status")]
    status: String,
    #[table(title = "Attempts")]
    attempts: i64,
    #[table(title = "Updated (UTC)")]
    updated_at: String,
    #[table(title = "Last error")]
    last_error: String,
}

impl From<EventOutput> for EventTableItem {
    fn from(e: EventOutput) -> Self {
        // Show updated_at when we have it (post-delivery), otherwise created_at
        // — for a brand-new event the row's update column is still null.
        let when = e.updated_at.unwrap_or_else(|| e.created_at.clone());
        EventTableItem {
            id: e.id,
            kind: e.kind,
            status: render_status(&e.status),
            attempts: e.attempts,
            updated_at: style::format_date(&when),
            last_error: e.last_error.unwrap_or_default(),
        }
    }
}

fn render_status(status: &str) -> String {
    // StatusColour only exposes Green/Red — pending stays plain so it doesn't
    // visually compete with the terminal outcomes (delivered, dead).
    match status {
        "delivered" => style::status_custom("delivered", style::StatusColour::Green),
        "dead" => style::status_custom("dead", style::StatusColour::Red),
        other => other.to_string(),
    }
}

pub(crate) async fn execute(
    args: &ArgMatches,
    mut configuration: Config,
    client: &reqwest::Client,
) {
    let id = args
        .get_one::<String>("id")
        .expect("id is required")
        .clone();
    let follow = args.get_flag("follow");

    let api_url = configuration.get_api_url();
    let auth_config = load_auth_config(configuration.name.clone());
    let endpoint = format!("{}/webhooks/{}/events", api_url, id);

    // First fetch: in --follow mode it also primes `seen_ids` so we only
    // surface *new* events on subsequent ticks instead of redrawing the
    // initial page every two seconds.
    let initial: Vec<EventOutput> = match fetch(client, &endpoint, &auth_config.token, &id).await {
        Ok(events) => events,
        Err(code) => exit_code::from_http_status(code).exit(),
    };

    let items: Vec<EventTableItem> = initial.iter().cloned().map(EventTableItem::from).collect();
    style::print_table(items.with_title());

    if !follow {
        return;
    }

    // Polling fallback for --follow: the server doesn't expose SSE yet, but a
    // 2s poll is good enough for a debugging aid. Dedupe by id so we don't
    // reprint the same delivered event every tick once it settles.
    let mut seen: std::collections::HashSet<String> = initial.into_iter().map(|e| e.id).collect();

    loop {
        tokio::time::sleep(FOLLOW_INTERVAL).await;
        let fresh = match fetch(client, &endpoint, &auth_config.token, &id).await {
            Ok(events) => events,
            Err(_) => continue, // transient: keep polling instead of dying
        };
        let new_ones: Vec<EventTableItem> = fresh
            .into_iter()
            .filter(|e| seen.insert(e.id.clone()))
            .map(EventTableItem::from)
            .collect();
        if !new_ones.is_empty() {
            style::print_table(new_ones.with_title());
        }
    }
}

async fn fetch(
    client: &reqwest::Client,
    endpoint: &str,
    token: &str,
    webhook_id: &str,
) -> Result<Vec<EventOutput>, u16> {
    let response = match client
        .get(endpoint)
        .header("Authorization", format!("Bearer {}", token))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Failed to fetch webhook events: {}", e);
            exit_code::from_reqwest_error(&e).exit();
        }
    };

    let status = response.status();
    if status != 200 {
        style::print_error(&http_error(status.as_u16(), "webhook", webhook_id));
        return Err(status.as_u16());
    }

    match response.json::<Vec<EventOutput>>().await {
        Ok(events) => Ok(events),
        Err(e) => {
            eprintln!("Failed to parse events: {}", e);
            exit_code::ExitCode::General.exit();
        }
    }
}
