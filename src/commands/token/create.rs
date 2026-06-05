use crate::cli::problem_json::render_response_error;
use crate::cli::style;
use crate::config::auth::load_auth_config;
use crate::config::config::Config;
use crate::exit_code;
use chrono::{Duration, Utc};
use clap::Arg;
use clap::ArgAction;
use clap::ArgMatches;
use clap::Command;
use serde::{Deserialize, Serialize};

pub(crate) fn command_config() -> Command {
    Command::new("create")
        .about("Create a scoped API token")
        .arg(Arg::new("name").required(true).help("Token name (label)"))
        .arg(
            Arg::new("scope")
                .short('s')
                .long("scope")
                .required(true)
                .action(ArgAction::Append)
                .help("Scope (verb:resource), repeatable. E.g. --scope deployments:read"),
        )
        .arg(
            Arg::new("namespace")
                .short('n')
                .long("namespace")
                .action(ArgAction::Append)
                .help("Restrict to a namespace, repeatable. Omit for all namespaces"),
        )
        .arg(
            Arg::new("expires")
                .short('e')
                .long("expires")
                .help("Expiry as a duration: 30d, 12h, 90m (omit for no expiry)"),
        )
}

#[derive(Serialize)]
struct TokenInput {
    name: String,
    scopes: Vec<String>,
    namespaces: Vec<String>,
    expire_at: Option<String>,
}

#[derive(Deserialize)]
struct TokenCreated {
    token: String,
    token_prefix: String,
    scopes: Vec<String>,
    namespaces: Vec<String>,
    expire_at: Option<String>,
}

/// Parse a human duration (`30d`, `12h`, `90m`) into an absolute RFC 3339
/// expiry timestamp. Returns Err with a human message on a bad value.
fn parse_expiry(spec: &str) -> Result<String, String> {
    let spec = spec.trim();
    let (num, unit) = spec.split_at(spec.find(|c: char| !c.is_ascii_digit()).ok_or_else(|| {
        format!(
            "invalid duration '{}': expected a number + unit (d/h/m)",
            spec
        )
    })?);
    let value: i64 = num
        .parse()
        .map_err(|_| format!("invalid duration '{}': '{}' is not a number", spec, num))?;
    let delta = match unit {
        "d" => Duration::days(value),
        "h" => Duration::hours(value),
        "m" => Duration::minutes(value),
        _ => return Err(format!("invalid duration unit '{}': use d, h or m", unit)),
    };
    Ok((Utc::now() + delta).to_rfc3339())
}

pub(crate) async fn execute(
    args: &ArgMatches,
    mut configuration: Config,
    client: &reqwest::Client,
) {
    let name = args.get_one::<String>("name").unwrap();
    let scopes: Vec<String> = args
        .get_many::<String>("scope")
        .map(|v| v.cloned().collect())
        .unwrap_or_default();
    let namespaces: Vec<String> = args
        .get_many::<String>("namespace")
        .map(|v| v.cloned().collect())
        .unwrap_or_default();

    let expire_at = match args.get_one::<String>("expires") {
        Some(spec) => match parse_expiry(spec) {
            Ok(ts) => Some(ts),
            Err(msg) => {
                style::print_error(&format!("error: {}", msg));
                exit_code::ExitCode::General.exit();
            }
        },
        None => None,
    };

    let api_url = configuration.get_api_url();
    let auth_config = load_auth_config(configuration.name.clone());

    let input = TokenInput {
        name: name.clone(),
        scopes,
        namespaces,
        expire_at,
    };

    let request = client
        .post(format!("{}/tokens", api_url))
        .header("Authorization", format!("Bearer {}", auth_config.token))
        .json(&input)
        .send()
        .await;

    match request {
        Ok(response) => {
            let status = response.status();
            if status.is_success() {
                let created: TokenCreated = response.json().await.unwrap();
                // The clear token is shown once. Print it on stdout so it can
                // be captured (e.g. `$(ring token create … )`), and the
                // can't-show-again warning on stderr so it doesn't pollute the
                // captured value.
                eprintln!(
                    "Token '{}' created. Copy it now — it will not be shown again.",
                    name
                );
                eprintln!(
                    "  scopes:     {}",
                    if created.scopes.is_empty() {
                        "(none)".to_string()
                    } else {
                        created.scopes.join(", ")
                    }
                );
                eprintln!(
                    "  namespaces: {}",
                    if created.namespaces.is_empty() {
                        "all".to_string()
                    } else {
                        created.namespaces.join(", ")
                    }
                );
                eprintln!(
                    "  expires:    {}",
                    created.expire_at.as_deref().unwrap_or("never")
                );
                eprintln!("  prefix:     {}", created.token_prefix);
                println!("{}", created.token);
            } else {
                let context = format!("Failed to create token '{}'", name);
                let code = render_response_error(&context, response).await;
                exit_code::from_http_status(code).exit();
            }
        }
        Err(error) => {
            eprintln!("Failed to create token: {}", error);
            exit_code::from_reqwest_error(&error).exit();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse_expiry;

    #[test]
    fn parses_days_hours_minutes() {
        assert!(parse_expiry("30d").is_ok());
        assert!(parse_expiry("12h").is_ok());
        assert!(parse_expiry("90m").is_ok());
    }

    #[test]
    fn rejects_bad_durations() {
        assert!(parse_expiry("30").is_err()); // no unit
        assert!(parse_expiry("d").is_err()); // no number
        assert!(parse_expiry("10y").is_err()); // unknown unit
        assert!(parse_expiry("abc").is_err());
    }

    #[test]
    fn produced_expiry_is_in_the_future() {
        let ts = parse_expiry("1d").unwrap();
        let parsed = chrono::DateTime::parse_from_rfc3339(&ts).unwrap();
        assert!(parsed > chrono::Utc::now());
    }
}
