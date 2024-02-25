use clap::{Command};
use clap::Arg;
use clap::ArgMatches;
use cli_table::{format::Justify, print_stdout, Table, WithTitle};
use serde_json::Result;
use crate::config::config::Config;
use crate::api::dto::deployment::DeploymentDTO;
use crate::config::config::load_auth_config;

pub(crate) fn command_config<'a, 'b>() -> Command {
    Command::new("list")
        .about("List deployments")
        .arg(
            Arg::new("namespace")
                .short('n')
                .long("namespace")
                .help("restrict only namespace")
        )
}

#[derive(Table)]
struct DeploymentTableItem {
    #[table(title = "ID", justify = "Justify::Right")]
    id: String,
    #[table(title = "Created at")]
    created_at: String,
    #[table(title = "Namespace")]
    namespace: String,
    #[table(title = "Name")]
    name: String,
    #[table(title = "Image")]
    image: String,
    #[table(title = "Runtime")]
    runtime: String,
    #[table(title = "Kind")]
    kind: String,
    #[table(title = "Replicas")]
    replicas: String,
    #[table(title = "Status")]
    status: String
}

pub(crate) fn execute(args: &ArgMatches, mut configuration: Config) {
    let mut deployments = vec![];
    let api_url = configuration.get_api_url();
    let auth_config = load_auth_config(configuration.name.clone());
    let mut query = format!("{}/deployments", api_url);

    if args.contains_id("namespace"){
        let namespace = args.get_one::<String>("namespace").unwrap();
        query.push_str(&format!("?namespace={}", namespace));
    }

    let response = ureq::get(&*query)
        .set("Authorization", &format!("Bearer {}", auth_config.token))
        .send_json({});
    let response_content = response.unwrap().into_string().unwrap();

    let value: Result<Vec<DeploymentDTO>> = serde_json::from_str(&response_content);
    let deployments_list = value.unwrap();

    for deployment in deployments_list {

        deployments.push(
            DeploymentTableItem {
                id: deployment.id,
                created_at: deployment.created_at,
                namespace: deployment.namespace,
                name: deployment.name,
                image: deployment.image,
                runtime: deployment.runtime,
                kind: deployment.kind,
                replicas: format!("{}/{}", deployment.instances.len(), deployment.replicas),
                status: deployment.status,
            },
        )
    }

    print_stdout(deployments.with_title()).expect("");
}
