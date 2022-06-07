use clap::App;
use clap::SubCommand;
use clap::ArgMatches;
use crate::config::config::Config;
use crate::models::deployments;
use cli_table::{format::Justify, print_stdout, Table, WithTitle};
use rusqlite::Connection;
use std::sync::{Mutex, Arc};
use chrono::NaiveDateTime;

pub(crate) fn command_config<'a, 'b>() -> App<'a, 'b> {
    SubCommand::with_name("deployment list")
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
}

pub(crate) fn list(_args: &ArgMatches, storage: Connection) {
    let mut deployments = vec![];

    let connection = Arc::new(Mutex::new(storage));
    let arc = Arc::clone(&connection);

    let guard = arc.lock().unwrap();

    let list_deployments = deployments::find_all(guard);
    for deployment in list_deployments {
        deployments.push(
            DeploymentItem {
                id: deployment.id,
                created_at: NaiveDateTime::from_timestamp(deployment.created_at, 0).to_string(),
                namespace: deployment.namespace,
                name: deployment.name,
                image: deployment.image,
                runtime: deployment.runtime,
                replicas: deployment.replicas,
            },
        )
    }

    print_stdout(deployments.with_title());
}

pub(crate) fn inspect(_args: &ArgMatches, storage: Connection) {

}