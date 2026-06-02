use clap::Arg;
use clap::ArgAction;
use clap::ArgMatches;
use clap::Command;

use crate::cli::problem_json::http_error;
use crate::cli::style;
use crate::config::config::Config;
use crate::config::config::load_auth_config;
use crate::exit_code;

pub(crate) fn command_config() -> Command {
    Command::new("delete")
        .about("Delete one or more deployments")
        .arg(
            Arg::new("id")
                .required(true)
                .num_args(1..)
                .action(ArgAction::Append)
                .help("One or more deployment ids to delete"),
        )
}

pub(crate) async fn execute(
    args: &ArgMatches,
    mut configuration: Config,
    client: &reqwest::Client,
) {
    let ids: Vec<&String> = args.get_many::<String>("id").unwrap().collect();
    let api_url = configuration.get_api_url();
    let auth_config = load_auth_config(configuration.name.clone());

    let mut had_error = false;

    // Attempt every id even if some fail, then let the exit code reflect
    // whether anything failed. A batch delete shouldn't abort halfway and
    // leave the operator guessing which ids went through.
    for id in ids {
        let request = client
            .delete(format!("{}/deployments/{}", api_url, id))
            .header("Authorization", format!("Bearer {}", auth_config.token))
            .send()
            .await;

        match request {
            Ok(response) => {
                let status = response.status();
                if status == 204 {
                    style::print_success(&format!("Deployment {} deleted", id));
                } else {
                    style::print_error(&http_error(status.as_u16(), "deployment", id));
                    had_error = true;
                }
            }
            Err(err) => {
                eprintln!("Cannot delete deployment {}: {}", id, err);
                had_error = true;
            }
        }
    }

    if had_error {
        exit_code::ExitCode::General.exit();
    }
}
