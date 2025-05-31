use clap::{Arg, ArgMatches, Command};
use crate::config::config::Config;
use crate::config::config::load_auth_config;
use crate::runtime::runtime::Log;

pub(crate) fn command_config<'a, 'b>() -> Command {
    Command::new("logs")
        .about("Show information on a deployment")
        .arg(
            Arg::new("id")
        )
}

pub(crate) async fn execute(args: &ArgMatches, mut configuration: Config) {
    let id = args.get_one::<String>("id").unwrap();
    let api_url = configuration.get_api_url();
    let auth_config = load_auth_config(configuration.name.clone());

    let request = ureq::get(&format!("{}/deployments/{}/logs", api_url, id))
        .set("Authorization", &format!("Bearer {}", auth_config.token))
        .set("Content-Type", "application/json")
        .call();

    match request {
        Ok(response) => {
            if response.status() != 200 {
                return eprintln!("Unable to fetch deployment logs: {}", response.status());
            }

            let logs: Vec<Log> = response.into_json::<Vec<Log>>().unwrap_or(vec![]);

            for log in logs {
                println!("{}", log.message);
            }
        },
        Err(_) => {
            eprintln!("Error fetching deployment logs");
        }
    }
}
