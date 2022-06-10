use clap::App;
use clap::Arg;
use clap::SubCommand;
use clap::ArgMatches;
use crate::models::deployments;
use rusqlite::Connection;
use std::sync::{Mutex, Arc};
use chrono::NaiveDateTime;
use std::collections::HashMap;
use shiplift::Docker;

pub(crate) fn command_config<'a, 'b>() -> App<'a, 'b> {
    SubCommand::with_name("deployment:inspect")
        .arg(
            Arg::with_name("id")
        )
}

#[tokio::main]
pub(crate) async fn execute(args: &ArgMatches<'_>, storage: Connection) {
    let id = args.value_of("id").unwrap();

    let connection = Arc::new(Mutex::new(storage));
    let arc = Arc::clone(&connection);

    let guard = arc.lock().unwrap();

    let deployment = deployments::find(guard, id.to_string()).unwrap().unwrap();
    let labels: HashMap<String, String> = deployments::Deployment::deserialize_labels(&deployment.labels);

    let mut instances: Vec<String> = vec![];
    let docker = Docker::new();

    match docker.containers().list(&Default::default()).await {
        Ok(containers) => {
            for container in containers {
                let container_id = &container.id;

                for (label, value) in container.labels.into_iter() {
                    if "ring_deployment" == label && value == id {
                        instances.push(container_id.to_string());
                    }
                }
            }
        }
        Err(e) => eprintln!("Error: {}", e),
    }

    println!("Name: {}", deployment.name);
    println!("Namespace: {}", deployment.namespace);
    println!("Created AT: {}", NaiveDateTime::from_timestamp(deployment.created_at, 0).to_string());

    println!("Labels:");
    for label in labels {
        println!("  {:?} = {:?}", label.0, label.1)
    }

    println!("Containers:");
    for instance in instances {
        println!("  {:?}", instance)
    }
}
