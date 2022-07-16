use clap::App;
use clap::Arg;
use clap::SubCommand;
use clap::ArgMatches;
use serde_json::Result;
use crate::config::config::Config;
use crate::config::config::load_auth_config;

pub(crate) fn command_config<'a, 'b>() -> App<'a, 'b> {
    SubCommand::with_name("deployment:delete")
        .arg(
            Arg::with_name("id")
        )
}

pub(crate) async fn execute(args: &ArgMatches<'_>, mut configuration: Config) {
    let id = args.value_of("id").unwrap();
    let api_url = configuration.get_api_url();
    let auth_config = load_auth_config();

    let request = ureq::delete(&format!("{}/deployments/{}", api_url, id))
        .set("Authorization", &format!("Bearer {}", auth_config.token))
        .send_json({});

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
