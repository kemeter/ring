use clap::{Command};
use clap::Arg;
use clap::ArgMatches;

use crate::config::config::Config;
use crate::config::config::load_auth_config;

pub(crate) fn command_config<'a, 'b>() -> Command {
    Command::new("delete")
        .about("Delete config")
        .arg(
            Arg::new("id")
                .required(true)
                .help("Config ID")
        )
}

pub(crate) async fn execute(args: &ArgMatches, mut configuration: Config, client: &reqwest::Client) {
    let id = args.get_one::<String>("id").unwrap();
    let api_url = configuration.get_api_url();
    let auth_config = load_auth_config(configuration.name.clone());

    let deployments: Vec<&str> = id.split(" ").collect();

    for deployment in deployments {
        let request = client
            .delete(&format!("{}/configs/{}", api_url, deployment))
            .header("Authorization", format!("Bearer {}", auth_config.token))
            .send()
            .await;

        match request {
            Ok(response) => {
                if response.status() == 204 {
                    return println!("Config {} deleted ", id);
                }

                println!("Cannot delete Config {}", id);
            }
            Err(_) => {
                println!("Cannot delete Config {}", id);
            }
        }
    }
}
