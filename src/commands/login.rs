use crate::cli::problem_json::{render_response_error, transport_error};
use crate::cli::style;
use crate::config::config::Config;
use crate::exit_code;
use clap::Arg;
use clap::ArgMatches;
use clap::Command;
use serde_json::json;
use std::collections::HashMap;
use std::fs;
use std::io::IsTerminal;

use crate::config::auth::AuthToken;
use crate::config::config::get_config_dir;
use std::string::String;

pub(crate) fn command_config() -> Command {
    Command::new("login")
        .about("Login to your account")
        .arg(
            Arg::new("username")
                .short('u')
                .long("username")
                .help("Your username (prompted if omitted)"),
        )
        .arg(
            Arg::new("password")
                .short('p')
                .long("password")
                .help("Your password (prompted if omitted)"),
        )
}

/// What to do with a credential given the flag value and whether stdin is a tty.
#[derive(Debug, PartialEq)]
enum CredentialAction {
    /// The flag was supplied; use it as-is.
    Use(String),
    /// The flag was missing but we have a terminal; prompt the user.
    Prompt,
    /// The flag was missing and there is no terminal; refuse.
    Fail,
}

/// Pure decision: flag present wins; otherwise prompt on a tty, fail without one.
fn decide_credential(flag: Option<String>, is_tty: bool) -> CredentialAction {
    match flag {
        Some(value) => CredentialAction::Use(value),
        None if is_tty => CredentialAction::Prompt,
        None => CredentialAction::Fail,
    }
}

/// Returns the username from the flag, or prompts for it on an interactive
/// terminal. Exits with a clear error when neither is available (pipe/CI).
fn resolve_username(flag: Option<String>) -> String {
    match decide_credential(flag, std::io::stdin().is_terminal()) {
        CredentialAction::Use(value) => value,
        CredentialAction::Fail => {
            style::print_error("error: username required; pass --username or run in a terminal");
            exit_code::ExitCode::General.exit();
        }
        CredentialAction::Prompt => inquire::Text::new("Username:")
            .prompt()
            .unwrap_or_else(|_| {
                eprintln!("Aborted.");
                exit_code::ExitCode::General.exit();
            }),
    }
}

/// Returns the password from the flag, or prompts for it (masked) on an
/// interactive terminal. Exits with a clear error when neither is available.
fn resolve_password(flag: Option<String>) -> String {
    match decide_credential(flag, std::io::stdin().is_terminal()) {
        CredentialAction::Use(value) => value,
        CredentialAction::Fail => {
            style::print_error("error: password required; pass --password or run in a terminal");
            exit_code::ExitCode::General.exit();
        }
        CredentialAction::Prompt => inquire::Password::new("Password:")
            .with_display_mode(inquire::PasswordDisplayMode::Masked)
            .without_confirmation()
            .prompt()
            .unwrap_or_else(|_| {
                eprintln!("Aborted.");
                exit_code::ExitCode::General.exit();
            }),
    }
}

pub(crate) async fn execute(
    args: &ArgMatches,
    mut configuration: Config,
    client: &reqwest::Client,
) {
    let username = resolve_username(args.get_one::<String>("username").cloned());
    let password = resolve_password(args.get_one::<String>("password").cloned());

    let config_directory = get_config_dir();
    let config_file = format!("{}/auth.json", config_directory);

    let base_url = configuration.get_api_url();
    let api_url = format!("{}/login", base_url);
    let request = client
        .post(&api_url)
        .json(&json!({
            "username": username,
            "password": password
        }))
        .send()
        .await;

    match request {
        Ok(response) => {
            let status = response.status();
            if status != 200 {
                let code = render_response_error("Login failed", response).await;
                exit_code::from_http_status(code).exit();
            }

            let auth = match response.json::<AuthToken>().await {
                Ok(a) => a,
                Err(e) => {
                    eprintln!("Failed to parse authentication response: {}", e);
                    exit_code::ExitCode::General.exit();
                }
            };

            let auth_file_content =
                fs::read_to_string(config_file.clone()).unwrap_or_else(|_| "{}".to_string());

            let mut context_auth: HashMap<String, AuthToken> =
                serde_json::from_str(&auth_file_content).unwrap_or_default();

            context_auth.insert(configuration.name, auth);

            let serialized_data = match serde_json::to_string(&context_auth) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("Failed to serialize auth data: {}", e);
                    exit_code::ExitCode::General.exit();
                }
            };

            if let Err(e) = fs::create_dir_all(&config_directory) {
                eprintln!("Failed to create config directory: {}", e);
                exit_code::ExitCode::General.exit();
            }

            if let Err(e) = fs::write(config_file, serialized_data) {
                eprintln!("Failed to write auth file: {}", e);
                exit_code::ExitCode::General.exit();
            }
            style::print_success(&format!("Logging in as {}", username));
        }
        Err(err) => {
            style::print_error(&transport_error(&err, &base_url));
            exit_code::from_reqwest_error(&err).exit();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CredentialAction, decide_credential};

    #[test]
    fn flag_value_is_used_regardless_of_tty() {
        assert_eq!(
            decide_credential(Some("admin".to_string()), true),
            CredentialAction::Use("admin".to_string())
        );
        assert_eq!(
            decide_credential(Some("admin".to_string()), false),
            CredentialAction::Use("admin".to_string())
        );
    }

    #[test]
    fn missing_flag_prompts_on_a_terminal() {
        assert_eq!(decide_credential(None, true), CredentialAction::Prompt);
    }

    #[test]
    fn missing_flag_fails_without_a_terminal() {
        assert_eq!(decide_credential(None, false), CredentialAction::Fail);
    }
}
