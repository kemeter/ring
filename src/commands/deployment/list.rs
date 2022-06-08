use clap::App;
use clap::Arg;
use clap::SubCommand;
use clap::ArgMatches;
use crate::models::deployments;
use cli_table::{format::Justify, print_stdout, Table, WithTitle};
use rusqlite::Connection;
use std::sync::{Mutex, Arc};
use chrono::NaiveDateTime;

pub(crate) fn command_config<'a, 'b>() -> App<'a, 'b> {
    SubCommand::with_name("deployment:list")
        .arg(
            Arg::with_name("namespace")
                .short("n")
                .long("namespace")
                .help("restrict only namespace")
                .takes_value(true)
        )
}

#[derive(Table)]
struct DeploymentItem {
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
    #[table(title = "Replicas")]
    replicas: i64,
    #[table(title = "Status")]
    status: String
}

pub(crate) fn execute(args: &ArgMatches, storage: Connection) {
    let mut deployments = vec![];
    let connection = Arc::new(Mutex::new(storage));
    let arc = Arc::clone(&connection);

    let guard = arc.lock().unwrap();

    let list_deployments = deployments::find_all(guard);
    for deployment in list_deployments {

        if args.is_present("namespace") {
            let namespace = args.value_of("namespace").unwrap();

            if namespace != deployment.namespace {
                continue;
            }
        }

        deployments.push(
            DeploymentItem {
                id: deployment.id,
                created_at: NaiveDateTime::from_timestamp(deployment.created_at, 0).to_string(),
                namespace: deployment.namespace,
                name: deployment.name,
                image: deployment.image,
                runtime: deployment.runtime,
                replicas: deployment.replicas,
                status: deployment.status,
            },
        )
    }

    print_stdout(deployments.with_title());
}
