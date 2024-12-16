use clap::ArgMatches;
use crate::config::config::{load_auth_config, Config};
use clap::{Command};
use clap::Arg;
use crate::api::dto::deployment::DeploymentOutput;

pub(crate) fn command_config<'a, 'b>() -> Command {
    Command::new("prune")
        .about("Delete all deployment")
        .arg(
            Arg::new("name")
        )
}

pub(crate) async fn execute(_args: &ArgMatches, mut configuration: Config) {
    let api_url = configuration.get_api_url();
    let auth_config = load_auth_config(configuration.name.clone());
    let query = format!("{}/deployments", api_url);

    let response = ureq::get(&*query)
        .set("Authorization", &format!("Bearer {}", auth_config.token))
        .set("Content-Type", "application/json")
        .call();

    let response_content = response.unwrap().into_string().unwrap();
    let value: serde_json::Result<Vec<DeploymentOutput>> = serde_json::from_str(&response_content);
    let deployments_list = value.unwrap();

    for deployment in deployments_list {
        let id = deployment.id;
        let request = ureq::delete(&format!("{}/deployments/{}", api_url, id))
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

