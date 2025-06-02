use crate::api::dto::deployment::DeploymentOutput;
use crate::config::config::load_auth_config;
use crate::config::config::Config;
use clap::Arg;
use clap::ArgMatches;
use clap::{ArgAction, Command};
use cli_table::{format::Justify, print_stdout, Table, WithTitle};

pub(crate) fn command_config<'a, 'b>() -> Command {
    Command::new("list")
        .about("List deployments")
        .arg(
            Arg::new("namespace")
                .short('n')
                .long("namespace")
                .help("restrict only namespace")
        )
        .arg(
            Arg::new("status")
                .action(ArgAction::Append)
                .short('s')
                .long("status")
                .help("Filter by status")
        )
}

#[derive(Table)]
struct DeploymentTableItem {
    #[table(title = "ID", justify = "Justify::Right")]
    id: String,
    #[table(title = "Created at")]
    created_at: String,
    #[table(title = "Updated at")]
    updated_at: String,
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
    let api_url = configuration.get_api_url();
    let auth_config = load_auth_config(configuration.name.clone());
    let mut query = format!("{}/deployments", api_url);
    let mut params: Vec<String> = Vec::new();

    if args.contains_id("namespace"){
        let namespace = args.get_many::<String>("namespace").unwrap();

        for namespace in namespace {
            params.push(format!("namespace[]={}", namespace));
        }
    }

    if args.contains_id("status"){
        let statuses = args.get_many::<String>("status").unwrap();
        for status in statuses {
            params.push(format!("status[]={}", status));
        }
    }

    if !params.is_empty() {
        query.push('?');
        query.push_str(&params.join("&"));
    }

    let request = ureq::get(&*query)
        .header("Authorization", &format!("Bearer {}", auth_config.token))
        .header("Content-Type", "application/json")
        .call();

    match request {
        Ok(mut response) => {
            if response.status() != 200 {
                return eprintln!("Unable to fetch deployments: {}", response.status());
            }

            let deployments_list: Vec<DeploymentOutput> = response.body_mut().read_json::<Vec<DeploymentOutput>>().unwrap();

            let mut deployments = vec![];
            for deployment in deployments_list {
                deployments.push(
                    DeploymentTableItem {
                        id: deployment.id,
                        created_at: deployment.created_at,
                        updated_at: deployment.updated_at,
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
        },
        Err(error) => {
            return eprintln!("Error fetching deployments: {}", error);
        }
    }
}
