use clap::Arg;
use clap::ArgMatches;
use clap::Command;

use crate::commands::problem_json::http_error;
use crate::commands::style;
use crate::config::config::Config;
use crate::config::config::load_auth_config;
use crate::exit_code;

pub(crate) fn command_config() -> Command {
    Command::new("delete")
        .about("Delete deployment")
        .arg(Arg::new("id"))
}

pub(crate) async fn execute(
    args: &ArgMatches,
    mut configuration: Config,
    client: &reqwest::Client,
) {
    let id = args.get_one::<String>("id").unwrap();
    let api_url = configuration.get_api_url();
    let auth_config = load_auth_config(configuration.name.clone());

    let deployments: Vec<&str> = id.split(" ").collect();

    for deployment in deployments {
        let request = client
            .delete(format!("{}/deployments/{}", api_url, deployment))
            .header("Authorization", format!("Bearer {}", auth_config.token))
            .send()
            .await;

        match request {
            Ok(response) => {
                let status = response.status();
                if status == 204 {
                    style::print_success(&format!("Deployment {} deleted", deployment));
                } else {
                    style::print_error(&http_error(status.as_u16(), "deployment", deployment));
                    exit_code::from_http_status(status.as_u16()).exit();
                }
            }
            Err(err) => {
                eprintln!("Cannot delete deployment {}: {}", deployment, err);
                exit_code::from_reqwest_error(&err).exit();
            }
        }
    }
}
