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

    let request = ureq::get(&*query)
        .header("Authorization", &format!("Bearer {}", auth_config.token))
        .header("Content-Type", "application/json")
        .call();

    match request {
        Ok(mut response) => {
            if response.status() != 200 {
                return eprintln!("Unable to fetch deployments: {}", response.status());
            }

            let deployments_list: Vec<DeploymentOutput> = response.body_mut().read_json::<Vec<DeploymentOutput>>().unwrap_or(vec![]);

            for deployment in deployments_list {
                let id = deployment.id;
                let request = ureq::delete(&format!("{}/deployments/{}", api_url, id))
                    .header("Authorization", &format!("Bearer {}", auth_config.token))
                    .header("Content-Type", "application/json")
                    .call();

                match request {
                    Ok(response) => {
                        if response.status() == 204 {
                            return println!("Deployment {} deleted ", id);
                        }
                    }
                    Err(_) => {
                        println!("Cannot delete deployment config");
                    }
                }
            }
        },
        Err(_) => {
            eprintln!("Error fetching deployments")
        }
    }
}

