use crate::cli::problem_json::transport_error;
use crate::cli::style;
use crate::config::auth::{AuthToken, load_auth_config};
use crate::config::config::Config;
use crate::config::config::get_config_dir;
use crate::exit_code;
use clap::ArgMatches;
use clap::Command;
use std::collections::HashMap;
use std::fs;

pub(crate) fn command_config() -> Command {
    Command::new("logout").about("Revoke the current session and forget the stored token")
}

pub(crate) async fn execute(
    _args: &ArgMatches,
    mut configuration: Config,
    client: &reqwest::Client,
) {
    let context = configuration.name.clone();
    let auth = load_auth_config(context.clone());

    // Revoke server-side first so the token can't be replayed. Best-effort: if
    // the server is unreachable or the token is already invalid we still drop
    // the local credential — the user asked to log out.
    if !auth.token.is_empty() {
        let base_url = configuration.get_api_url();
        let api_url = format!("{}/logout", base_url);
        match client
            .post(&api_url)
            .header("Authorization", format!("Bearer {}", auth.token))
            .send()
            .await
        {
            // The server reached us but refused the revoke (e.g. 500). Warn so a
            // failed server-side revoke isn't silently indistinguishable from a
            // clean 204 — but don't abort: the local token is still removed. A
            // 401 means the token was already invalid, which is a no-op success.
            Ok(response) => {
                let status = response.status();
                if !status.is_success() && status != reqwest::StatusCode::UNAUTHORIZED {
                    style::print_warning(&format!(
                        "server did not confirm revocation (HTTP {}); removing the local token anyway",
                        status.as_u16()
                    ));
                }
            }
            Err(err) => {
                // Surface the transport problem but don't abort: the local
                // token is still removed below.
                style::print_error(&transport_error(&err, &base_url));
            }
        }
    }

    // Remove just this context's entry from auth.json, leaving other contexts
    // intact. If the file or entry is already gone, that's a successful no-op.
    let config_file = format!("{}/auth.json", get_config_dir());
    if let Ok(content) = fs::read_to_string(&config_file) {
        let mut context_auth: HashMap<String, AuthToken> =
            serde_json::from_str(&content).unwrap_or_default();
        if context_auth.remove(&context).is_some() {
            match serde_json::to_string(&context_auth) {
                Ok(serialized) => {
                    if let Err(e) = fs::write(&config_file, serialized) {
                        eprintln!("Failed to update auth file: {}", e);
                        exit_code::ExitCode::General.exit();
                    }
                }
                Err(e) => {
                    eprintln!("Failed to serialize auth data: {}", e);
                    exit_code::ExitCode::General.exit();
                }
            }
        }
    }

    style::print_success(&format!("Logged out of context '{}'", context));
}
