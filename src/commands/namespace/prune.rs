use crate::api::dto::deployment::DeploymentOutput;
use crate::config::config::{Config, load_auth_config};
use crate::exit_code;
use clap::Arg;
use clap::ArgAction;
use clap::ArgMatches;
use clap::Command;

fn is_prunable(status: &str) -> bool {
    matches!(
        status,
        "completed"
            | "failed"
            | "deleted"
            | "CrashLoopBackOff"
            | "ImagePullBackOff"
            | "CreateContainerError"
            | "NetworkError"
            | "ConfigError"
            | "FileSystemError"
            | "Error"
    )
}

pub(crate) fn command_config() -> Command {
    Command::new("prune")
        .about("Delete stopped/failed deployments in a namespace")
        .arg(Arg::new("name"))
        .arg(
            Arg::new("all")
                .long("all")
                .short('a')
                .help("Delete all deployments, including running ones")
                .action(ArgAction::SetTrue),
        )
}

pub(crate) async fn execute(
    args: &ArgMatches,
    mut configuration: Config,
    client: &reqwest::Client,
) {
    let api_url = configuration.get_api_url();
    let auth_config = load_auth_config(configuration.name.clone());
    let namespace_filter = args.get_one::<String>("name");
    let prune_all = args.get_flag("all");

    let query = format!("{}/deployments", api_url);

    let request = client
        .get(&*query)
        .header("Authorization", format!("Bearer {}", auth_config.token))
        .send()
        .await;

    match request {
        Ok(response) => {
            let status = response.status();
            if status != 200 {
                eprintln!("Unable to fetch deployments: {}", status);
                exit_code::from_http_status(status.as_u16()).exit();
            }

            let deployments_list: Vec<DeploymentOutput> = response
                .json::<Vec<DeploymentOutput>>()
                .await
                .unwrap_or(vec![]);

            let mut deleted_count = 0;
            let mut error_count = 0;
            let mut skipped_count = 0;

            for deployment in deployments_list {
                // Filter by namespace if provided
                if let Some(namespace) = namespace_filter
                    && &deployment.namespace != namespace
                {
                    continue;
                }

                if !prune_all && !is_prunable(&deployment.status) {
                    skipped_count += 1;
                    continue;
                }

                let id = deployment.id;
                let request = client
                    .delete(format!("{}/deployments/{}", api_url, id))
                    .header("Authorization", format!("Bearer {}", auth_config.token))
                    .send()
                    .await;

                match request {
                    Ok(response) => {
                        if response.status() == 204 {
                            println!("Deployment {} deleted", id);
                            deleted_count += 1;
                        } else {
                            eprintln!(
                                "Failed to delete deployment {}: status {}",
                                id,
                                response.status()
                            );
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
            if skipped_count > 0 {
                println!(
                    "  Skipped (active): {} — use --all to delete them too",
                    skipped_count
                );
            }
            if error_count > 0 {
                println!("  Failed: {}", error_count);
                exit_code::ExitCode::General.exit();
            }
        }
        Err(err) => {
            eprintln!("Error fetching deployments: {}", err);
            exit_code::from_reqwest_error(&err).exit();
        }
    }
}
