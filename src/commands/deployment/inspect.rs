use clap::App;
use clap::Arg;
use clap::SubCommand;
use clap::ArgMatches;
use serde_json::Result;
use crate::config::config::Config;
use crate::api::dto::deployment::DeploymentDTO;
use crate::config::config::load_auth_config;

pub(crate) fn command_config<'a, 'b>() -> App<'a, 'b> {
    SubCommand::with_name("deployment:inspect")
        .arg(
            Arg::with_name("id")
        )
}

pub(crate) async fn execute(args: &ArgMatches<'_>, mut configuration: Config) {
    let id = args.value_of("id").unwrap();
    let api_url = configuration.get_api_url();
    let auth_config = load_auth_config();

    let response = ureq::get(&format!("{}/deployments/{}", api_url, id))
        .set("Authorization", &format!("Bearer {}", auth_config.token))
        .send_json({});
    let response_content = response.unwrap().into_string().unwrap();
    let value: Result<DeploymentDTO> = serde_json::from_str(&response_content);
    let deployment = value.unwrap();

    println!("Name: {}", deployment.name);
    println!("Namespace: {}", deployment.namespace);
    println!("Image: {}", deployment.image);
    println!("Replicas: {}", deployment.replicas);
    println!("Created AT: {}", deployment.created_at);

    println!("Labels:");
    for label in deployment.labels {
        println!("  {:?} = {:?}", label.0, label.1)
    }

    println!("Instances:");
    for instance in deployment.instances {
        println!("  {:?}", instance)
    }
}
