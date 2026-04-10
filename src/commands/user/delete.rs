use crate::config::config::Config;
use crate::config::config::load_auth_config;
use crate::exit_code;
use clap::Arg;
use clap::ArgMatches;
use clap::Command;

pub(crate) fn command_config() -> Command {
    Command::new("delete")
        .about("Delete user")
        .arg(Arg::new("id"))
}

pub(crate) async fn execute(
    args: &ArgMatches,
    mut configuration: Config,
    client: &reqwest::Client,
) {
    let api_url = configuration.get_api_url();
    let auth_config = load_auth_config(configuration.name.clone());

    let id = args.get_one::<String>("id").unwrap();

    let request = client
        .delete(format!("{}/users/{}", api_url, id))
        .header("Authorization", format!("Bearer {}", auth_config.token))
        .send()
        .await;

    match request {
        Ok(response) => {
            let status = response.status();
            if status == 204 {
                println!("User {} deleted ", id)
            } else {
                eprintln!("Cannot delete user: {}", status);
                exit_code::from_http_status(status.as_u16()).exit();
            }
        }
        Err(err) => {
            eprintln!("Cannot delete user: {}", err);
            exit_code::from_reqwest_error(&err).exit();
        }
    }
}
