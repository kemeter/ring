use warp::Filter;
use warp::http::StatusCode;
use serde::{Serialize, Deserialize};
use rusqlite::Connection;
use crate::models::pods;
use uuid::Uuid;
use log::info;
use std::sync::{Mutex, Arc};
use chrono::{NaiveDateTime, DateTime, Utc};
use std::collections::HashMap;

#[tokio::main]
pub(crate) async fn start(storage: Arc<Mutex<Connection>>, server_address: &str)
{
    info!("Starting server on {}", server_address);

    let conn = Arc::clone(&storage);
    let conn2 = Arc::clone(&storage);

    let list = warp::get()
        .and(warp::path("pods"))
        .map(move || {
            println!("List pods");
            let mut pods: Vec<PodOutput> = Vec::new();
            let guard = conn.lock().unwrap();

            let list_pods = pods::find_all(guard);
            for pod in list_pods.into_iter() {
                let output = hydrate_output(pod);

                pods.push(output);
            }

            warp::reply::json(&pods)
        });

    let post = warp::post()
        .and(warp::path("pods"))
        .and(warp::body::json())
        .map(move |pod_input: PodInput| {

            let mut filters = Vec::new();
            filters.push(pod_input.namespace.clone());
            filters.push(pod_input.name.clone());

            let guard = conn2.lock().unwrap();
            let option = pods::find_one_by_filters(&guard, filters);
            let config = option.as_ref().unwrap();

            // pod found
            if config.is_some() {
                info!("Found pod");
                let mut pod = config.clone().unwrap();
                println!("{:?}", pod);
                println!("image: {:?}  image: {:?}", pod_input.image.clone(), pod.image);

                //@todo: implement reel pod diff
                if pod_input.image.clone() != pod.image {
                    info!("Image changed");
                    println!("Image changed");

                    pod.status = "delete".to_string();
                    pods::update(&guard, &pod);

                    pod.image = pod_input.image.clone();
                    pods::create(&guard, &pod);

                    println!("{:?}", pod);
                }

                let pod_output = hydrate_output(pod);

                return warp::reply::with_status(warp::reply::json(&pod_output), StatusCode::OK);

            }  else {
                info!("Pod not found, create a new one");

                let utc: DateTime<Utc> = Utc::now();
                let pod = pods::Pod {
                    id: Uuid::new_v4().to_string(),
                    name: pod_input.name.clone(),
                    runtime: pod_input.runtime.clone(),
                    namespace: pod_input.namespace.clone(),
                    image: pod_input.image.clone(),
                    status: "running".to_string(),
                    created_at: utc.timestamp(),
                    labels: pod_input.labels,
                    instances: [].to_vec(),
                    replicas: pod_input.replicas,
                };

                pods::create(&guard, &pod);

                let pod_output = hydrate_output(pod);

                return warp::reply::with_status(warp::reply::json(&pod_output), StatusCode::CREATED);
            }
        });

    let routes = list.or(post);

    warp::serve(routes).run(([0,0,0,0], 3030)).await
}

#[derive(Deserialize, Serialize, Debug, Clone)]
struct PodInput {
    name: String,
    runtime: String,
    namespace: String,
    image: String,
    replicas: i64,
    labels: String
}

#[derive(Deserialize, Serialize, Debug, Clone)]
struct PodOutput {
    id: String,
    created_at: String,
    status: String,
    name: String,
    runtime: String,
    namespace: String,
    image: String,
    replicas: u8,
    ports: Vec<String>,
    labels: HashMap<String, String>
}

fn hydrate_output(pod: pods::Pod) -> PodOutput {
    let labels: HashMap<String, String> = pods::Pod::deserialize_labels(&pod.labels);

    return PodOutput{
        id: pod.id,
        created_at: NaiveDateTime::from_timestamp(pod.created_at, 0).to_string(),
        status: pod.status,
        name: pod.name,
        namespace: pod.namespace,
        runtime: pod.runtime,
        image: pod.image,
        replicas: 0,
        ports: [].to_vec(),
        labels: labels
    };
}
