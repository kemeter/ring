use crate::api::action::token::TokenView;
use crate::cli::problem_json::http_error_global_list;
use crate::cli::style;
use crate::config::config::{Config, load_auth_config};
use crate::exit_code;
use chrono::Utc;
use clap::ArgMatches;
use clap::Command;
use cli_table::{Table, WithTitle};

pub(crate) fn command_config() -> Command {
    Command::new("list").about("List your API tokens")
}

#[derive(Table)]
struct TokenTableItem {
    #[table(title = "Id")]
    id: String,
    #[table(title = "Name")]
    name: String,
    #[table(title = "Prefix")]
    prefix: String,
    #[table(title = "Scopes")]
    scopes: String,
    #[table(title = "Namespaces")]
    namespaces: String,
    #[table(title = "Status")]
    status: String,
    #[table(title = "Created (UTC)")]
    created_at: String,
    #[table(title = "Last used (UTC)")]
    last_used: String,
    #[table(title = "Expires (UTC)")]
    expires: String,
}

/// Derive a human status from the token's lifecycle fields. Revoked wins over
/// expired (it's the more deliberate end state); both render red, active green.
///
/// Matches the server's `Token::is_expired` fail-closed semantics: an
/// `expire_at` we can't parse is shown as `expired`, never `active` — a token
/// the client can't reason about must not be presented as usable.
fn status_label(revoked_at: &Option<String>, expire_at: &Option<String>) -> String {
    if revoked_at.is_some() {
        return style::status_custom("revoked", style::StatusColour::Red);
    }
    if let Some(exp) = expire_at {
        let expired = match chrono::DateTime::parse_from_rfc3339(exp) {
            Ok(ts) => ts <= Utc::now(),
            // Unparseable expiry → treat as expired (fail closed).
            Err(_) => true,
        };
        if expired {
            return style::status_custom("expired", style::StatusColour::Red);
        }
    }
    style::status_custom("active", style::StatusColour::Green)
}

pub(crate) async fn execute(
    _args: &ArgMatches,
    mut configuration: Config,
    client: &reqwest::Client,
) {
    let api_url = configuration.get_api_url();
    let auth_config = load_auth_config(configuration.name.clone());

    let request = client
        .get(format!("{}/tokens", api_url))
        .header("Authorization", format!("Bearer {}", auth_config.token))
        .send()
        .await;

    match request {
        Ok(response) => {
            let status = response.status();
            if status != 200 {
                style::print_error(&http_error_global_list(status.as_u16(), "tokens"));
                exit_code::from_http_status(status.as_u16()).exit();
            }

            let token_list: Vec<TokenView> = match response.json().await {
                Ok(list) => list,
                Err(e) => {
                    eprintln!("Failed to parse token list: {}", e);
                    exit_code::ExitCode::General.exit();
                }
            };

            let tokens: Vec<TokenTableItem> = token_list
                .into_iter()
                .map(|t| TokenTableItem {
                    id: t.id,
                    name: t.name,
                    prefix: t.token_prefix,
                    scopes: if t.scopes.is_empty() {
                        "-".to_string()
                    } else {
                        t.scopes.join(",")
                    },
                    namespaces: if t.namespaces.is_empty() {
                        "all".to_string()
                    } else {
                        t.namespaces.join(",")
                    },
                    status: status_label(&t.revoked_at, &t.expire_at),
                    last_used: t
                        .last_used_at
                        .map(|d| style::format_date(&d))
                        .unwrap_or_else(|| "never".to_string()),
                    expires: t
                        .expire_at
                        .map(|d| style::format_date(&d))
                        .unwrap_or_else(|| "never".to_string()),
                    created_at: style::format_date(&t.created_at),
                })
                .collect();

            style::print_table(tokens.with_title());
        }
        Err(error) => {
            eprintln!("Failed to fetch tokens: {}", error);
            exit_code::from_reqwest_error(&error).exit();
        }
    }
}
