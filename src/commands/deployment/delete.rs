use clap::{Command};
use clap::Arg;
use clap::ArgMatches;

use crate::config::config::Config;
use crate::config::config::load_auth_config;

pub(crate) fn command_config<'a, 'b>() -> Command {
    Command::new("delete")
        .about("Delete deployment")
        .arg(
            Arg::new("id")
        )
}

pub(crate) async fn execute(args: &ArgMatches, mut configuration: Config) {
    let id = args.get_one::<String>("id").unwrap();
    let api_url = configuration.get_api_url();
    let auth_config = load_auth_config(configuration.name.clone());

    let deployments: Vec<&str> = id.split(" ").collect();

    for deployment in deployments {
        let request = ureq::delete(&format!("{}/deployments/{}", api_url, deployment))
            .set("Authorization", &format!("Bearer {}", auth_config.token))
            .set("Content-Type", "application/json")
            .call();

        match request {
            Ok(response) => {
                if response.status() == 204 {
                    return println!("Deployment {} deleted ", id);
                }
            }
            Err(err) => {
                debug!("{:?}", err);
                println!("Cannot delete deployment config");
            }
        }
    }
}
