use crate::api::dto::deployment::DeploymentOutput;
use crate::cli::output::{output_arg, output_format};
use crate::cli::problem_json::http_error;
use crate::cli::style;
use crate::config::auth::load_auth_config;
use crate::config::config::Config;
use crate::exit_code;
use clap::Arg;
use clap::ArgMatches;
use clap::Command;
use cli_table::{Table, WithTitle};

pub(crate) fn command_config() -> Command {
    Command::new("inspect")
        .about("Show information on a deployment")
        .arg(Arg::new("id").help("Deployment ID").required(true))
        .arg(output_arg())
}

#[derive(Table)]
struct VolumeTable {
    #[table(title = "Type")]
    r#type: String,

    #[table(title = "Source")]
    source: String,

    #[table(title = "Destination")]
    destination: String,

    #[table(title = "Key")]
    key: String,

    #[table(title = "Driver")]
    driver: String,

    #[table(title = "Permission")]
    permission: String,
}

pub(crate) async fn execute(
    args: &ArgMatches,
    mut configuration: Config,
    client: &reqwest::Client,
) {
    let id = args.get_one::<String>("id").unwrap();
    let api_url = configuration.get_api_url();
    let auth_config = load_auth_config(configuration.name.clone());

    let request = client
        .get(format!("{}/deployments/{}", api_url, id))
        .header("Authorization", format!("Bearer {}", auth_config.token))
        .send()
        .await;

    match request {
        Ok(response) => {
            let status = response.status();
            if status != 200 {
                style::print_error(&http_error(status.as_u16(), "deployment", id));
                exit_code::from_http_status(status.as_u16()).exit();
            }

            let body = match response.text().await {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("Failed to read deployment response: {}", e);
                    exit_code::ExitCode::General.exit();
                }
            };

            if output_format(args).is_json() {
                println!("{}", body);
                return;
            }

            let deployment: DeploymentOutput = match serde_json::from_str(&body) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("Failed to parse deployment: {}", e);
                    exit_code::ExitCode::General.exit();
                }
            };

            // Main section
            println!("DEPLOYMENT DETAILS");
            println!("==================");
            println!("Name          : {}", deployment.name);
            println!("Namespace     : {}", deployment.namespace);
            println!("Kind          : {}", deployment.kind);
            println!("Image         : {}", deployment.image);
            println!("Replicas      : {}", deployment.replicas);
            println!("Restart count : {}", deployment.restart_count);
            println!("Created at    : {}", deployment.created_at);
            println!("Updated at    : {}", deployment.updated_at);
            println!();

            // Labels
            if !deployment.labels.is_empty() {
                println!("LABELS");
                println!("------");
                for (key, value) in deployment.labels {
                    println!("  {} = {}", key, value);
                }
                println!();
            }

            // Instances
            if !deployment.instances.is_empty() {
                println!("INSTANCES");
                println!("---------");
                for instance in deployment.instances {
                    match &instance.address {
                        Some(addr) => println!("  {} ({})", instance.id, addr),
                        None => println!("  {}", instance.id),
                    }
                }
                println!();
            }

            let mut volumes = vec![];

            for volume in deployment.volumes {
                volumes.push(VolumeTable {
                    r#type: volume.r#type,
                    source: volume.source.clone().unwrap_or_default(),
                    destination: volume.destination,
                    key: volume.key.unwrap_or_default(),
                    driver: volume.driver,
                    permission: volume.permission,
                });
            }

            style::print_table(volumes.with_title());
        }
        Err(err) => {
            eprintln!("Error fetching deployment: {}", err);
            exit_code::from_reqwest_error(&err).exit();
        }
    }
}
