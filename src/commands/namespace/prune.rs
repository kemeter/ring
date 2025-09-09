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

pub(crate) async fn execute(_args: &ArgMatches, mut configuration: Config, client: &reqwest::Client) {
    let api_url = configuration.get_api_url();
    let auth_config = load_auth_config(configuration.name.clone());
    let query = format!("{}/deployments", api_url);

    let request = client
        .get(&*query)
        .header("Authorization", format!("Bearer {}", auth_config.token))
        .send()
        .await;

    match request {
        Ok(response) => {
            if response.status() != 200 {
                return eprintln!("Unable to fetch deployments: {}", response.status());
            }

            let deployments_list: Vec<DeploymentOutput> = response.json::<Vec<DeploymentOutput>>().await.unwrap_or(vec![]);

            for deployment in deployments_list {
                let id = deployment.id;
                let request = client
                    .delete(&format!("{}/deployments/{}", api_url, id))
                    .header("Authorization", format!("Bearer {}", auth_config.token))
                    .send()
                    .await;

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

