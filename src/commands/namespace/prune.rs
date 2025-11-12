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

pub(crate) async fn execute(args: &ArgMatches, mut configuration: Config, client: &reqwest::Client) {
    let api_url = configuration.get_api_url();
    let auth_config = load_auth_config(configuration.name.clone());
    let namespace_filter = args.get_one::<String>("name");

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

            let mut deleted_count = 0;
            let mut error_count = 0;

            for deployment in deployments_list {
                // Filter by namespace if provided
                if let Some(namespace) = namespace_filter {
                    if &deployment.namespace != namespace {
                        continue;
                    }
                }

                let id = deployment.id;
                let request = client
                    .delete(&format!("{}/deployments/{}", api_url, id))
                    .header("Authorization", format!("Bearer {}", auth_config.token))
                    .send()
                    .await;

                match request {
                    Ok(response) => {
                        if response.status() == 204 {
                            println!("Deployment {} deleted", id);
                            deleted_count += 1;
                        } else {
                            eprintln!("Failed to delete deployment {}: status {}", id, response.status());
                            error_count += 1;
                        }
                    }
                    Err(e) => {
                        eprintln!("Cannot delete deployment {}: {}", id, e);
                        error_count += 1;
                    }
                }
            }

            println!("\nSummary:");
            println!("  Deleted: {}", deleted_count);
            if error_count > 0 {
                println!("  Failed: {}", error_count);
            }
        },
        Err(_) => {
            eprintln!("Error fetching deployments")
        }
    }
}

