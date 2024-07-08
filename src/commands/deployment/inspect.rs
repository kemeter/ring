use clap::{Command};
use clap::Arg;
use clap::ArgMatches;
use serde_json::Result;
use crate::config::config::Config;
use crate::api::dto::deployment::DeploymentDTO;
use crate::config::config::load_auth_config;

pub(crate) fn command_config<'a, 'b>() -> Command {
    Command::new("inspect")
        .about("Show information on a deployment")
        .arg(
            Arg::new("id")
        )
}

pub(crate) async fn execute(args: &ArgMatches<>, mut configuration: Config) {
    let id = args.get_one::<String>("id").unwrap();
    let api_url = configuration.get_api_url();
    let auth_config = load_auth_config(configuration.name.clone());

    let response = ureq::get(&format!("{}/deployments/{}", api_url, id))
        .set("Authorization", &format!("Bearer {}", auth_config.token))
        .set("Content-Type", "application/json")
        .call();
    let response_content = response.unwrap().into_string().unwrap();
    let value: Result<DeploymentDTO> = serde_json::from_str(&response_content);
    let deployment = value.unwrap();

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
}
