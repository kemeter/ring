use clap::Arg;
use clap::ArgMatches;
use clap::Command;

use crate::config::config::Config;
use crate::config::config::load_auth_config;
use crate::exit_code;

pub(crate) fn command_config() -> Command {
    Command::new("delete")
        .about("Delete config")
        .arg(Arg::new("id").required(true).help("Config ID"))
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
            .delete(format!("{}/configs/{}", api_url, deployment))
            .header("Authorization", format!("Bearer {}", auth_config.token))
            .send()
            .await;

        match request {
            Ok(response) => {
                let status = response.status();
                if status == 204 {
                    println!("Config {} deleted ", deployment);
                } else {
                    eprintln!("Cannot delete Config {}: {}", deployment, status);
                    exit_code::from_http_status(status.as_u16()).exit();
                }
            }
            Err(err) => {
                eprintln!("Cannot delete Config {}: {}", deployment, err);
                exit_code::from_reqwest_error(&err).exit();
            }
        }
    }
}
