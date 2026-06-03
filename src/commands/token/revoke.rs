use crate::cli::problem_json::render_response_error;
use crate::cli::style;
use crate::config::config::{Config, load_auth_config};
use crate::exit_code;
use clap::Arg;
use clap::ArgAction;
use clap::ArgMatches;
use clap::Command;
use std::io::IsTerminal;

pub(crate) fn command_config() -> Command {
    Command::new("revoke")
        .about("Revoke an API token")
        .arg(Arg::new("id").required(true).help("Token id"))
        .arg(
            Arg::new("yes")
                .short('y')
                .long("yes")
                .action(ArgAction::SetTrue)
                .help("Skip the confirmation prompt"),
        )
}

pub(crate) async fn execute(
    args: &ArgMatches,
    mut configuration: Config,
    client: &reqwest::Client,
) {
    let id = args.get_one::<String>("id").unwrap();
    let assume_yes = args.get_flag("yes");

    // Confirm interactively only when we have a tty and --yes wasn't given.
    // In a pipe/CI without --yes we refuse rather than silently revoking.
    if !assume_yes {
        if std::io::stdin().is_terminal() {
            let confirmed =
                inquire::Confirm::new(&format!("Revoke token '{}'? This cannot be undone.", id))
                    .with_default(false)
                    .prompt()
                    .unwrap_or(false);
            if !confirmed {
                eprintln!("Aborted.");
                return;
            }
        } else {
            style::print_error(
                "error: refusing to revoke without confirmation; pass --yes to proceed non-interactively",
            );
            exit_code::ExitCode::General.exit();
        }
    }

    let api_url = configuration.get_api_url();
    let auth_config = load_auth_config(configuration.name.clone());

    let request = client
        .delete(format!("{}/tokens/{}", api_url, id))
        .header("Authorization", format!("Bearer {}", auth_config.token))
        .send()
        .await;

    match request {
        Ok(response) => {
            let status = response.status();
            if status.is_success() {
                style::print_success(&format!("Token '{}' revoked", id));
            } else {
                let context = format!("Failed to revoke token '{}'", id);
                let code = render_response_error(&context, response).await;
                exit_code::from_http_status(code).exit();
            }
        }
        Err(error) => {
            eprintln!("Failed to revoke token: {}", error);
            exit_code::from_reqwest_error(&error).exit();
        }
    }
}
