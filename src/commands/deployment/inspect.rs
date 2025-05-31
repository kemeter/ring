use crate::api::dto::deployment::DeploymentDTO;
use crate::config::config::load_auth_config;
use crate::config::config::Config;
use clap::Arg;
use clap::ArgMatches;
use clap::Command;
use cli_table::{print_stdout, Table, WithTitle};

pub(crate) fn command_config<'a, 'b>() -> Command {
    Command::new("inspect")
        .about("Show information on a deployment")
        .arg(
            Arg::new("id")
        )
}

#[derive(Table)]
struct VolumeTable {
    source: String,
    destination: String,
    driver: String,
    permission: String,
}

pub(crate) async fn execute(args: &ArgMatches<>, mut configuration: Config) {
    let id = args.get_one::<String>("id").unwrap();
    let api_url = configuration.get_api_url();
    let auth_config = load_auth_config(configuration.name.clone());

    let request = ureq::get(&format!("{}/deployments/{}", api_url, id))
        .set("Authorization", &format!("Bearer {}", auth_config.token))
        .set("Content-Type", "application/json")
        .call();

    match request {
        Ok(response) => {
            if response.status() != 200 {
                return eprintln!("Unable to fetch deployment: {}", response.status());
            }

            let deployment = response.into_json::<DeploymentDTO>().unwrap();

            println!("Name: {}", deployment.name);
            println!("Namespace: {}", deployment.namespace);
            println!("Kind: {}", deployment.kind);
            println!("Image: {}", deployment.image);
            println!("Replicas: {}", deployment.replicas);
            println!("Restart count: {}", deployment.restart_count);
            println!("Created AT: {}", deployment.created_at);

            println!("Labels:");
            for label in deployment.labels {
                println!("  {:?} = {:?}", label.0, label.1)
            }

            println!("Instances:");
            for instance in deployment.instances {
                println!("  {:?}", instance)
            }

            println!("Environment:");
            for secret in deployment.secrets {
                println!("  {:?}: {:?}", secret.0, secret.1)
            }

            let mut volumes = vec![];

            for volume in deployment.volumes {
                volumes.push(VolumeTable {
                    source: volume.source,
                    destination: volume.destination,
                    driver: volume.driver,
                    permission: volume.permission,
                });

            }

            print_stdout(volumes.with_title()).expect("");
        },
        Err(_) => {
            eprintln!("Error from server (NotFound)");
        }
    }
}
