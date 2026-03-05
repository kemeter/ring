use clap::Command;
use clap::Arg;
use clap::ArgMatches;

use crate::config::config::Config;
use crate::config::config::load_auth_config;

pub(crate) fn command_config() -> Command {
    Command::new("delete")
        .about("Delete a secret")
        .arg(
            Arg::new("id")
                .required(true)
                .help("Secret ID")
        )
}

pub(crate) async fn execute(args: &ArgMatches, mut configuration: Config, client: &reqwest::Client) {
    let id = args.get_one::<String>("id").unwrap();
    let api_url = configuration.get_api_url();
    let auth_config = load_auth_config(configuration.name.clone());

    let request = client
        .delete(&format!("{}/secrets/{}", api_url, id))
        .header("Authorization", format!("Bearer {}", auth_config.token))
        .send()
        .await;

    match request {
        Ok(response) => {
            match response.status().as_u16() {
                204 => println!("Secret {} deleted", id),
                404 => println!("Secret {} not found", id),
                _ => println!("Failed to delete secret: {}", response.status()),
            }
        }
        Err(error) => {
            println!("Failed to delete secret: {}", error);
        }
    }
}
