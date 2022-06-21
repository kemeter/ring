use serde::{Serialize, Deserialize};
use rusqlite::Connection;
use uuid::Uuid;
use log::info;
use std::sync::Arc;
use chrono::{NaiveDateTime, DateTime, Utc};
use std::collections::HashMap;
use crate::models::deployments;
use crate::runtime::docker;

use std::{
    net::SocketAddr,
    time::Duration
};
use axum::{
    error_handling::HandleErrorLayer,
    extract::{Extension, Path},
    http::StatusCode,
    response::IntoResponse,
    routing::{get},
    Json, Router,
};

use tower::{BoxError, ServiceBuilder};
use tokio::sync::Mutex;
use crate::config::config::Config;

pub type Db = Arc<Mutex<Connection>>;

async fn deployment_list(Extension(connexion): Extension<Db>) -> impl IntoResponse {

    let mut deployments: Vec<DeploymentOutput> = Vec::new();
    let guard = connexion.lock().await;

    let list_deployments = deployments::find_all(guard);

    for deployment in list_deployments.into_iter() {
        let d = deployment.clone();

        let mut output = hydrate_deployment_output(deployment);
        let instances = docker::list_instances(d.id.to_string()).await;
        output.instances = instances;

        deployments.push(output);
    }

    Json(deployments)
}

async fn deployment_create(Json(input): Json<DeploymentInput>, Extension(connexion): Extension<Db>) -> impl IntoResponse {
    let mut filters = Vec::new();
    filters.push(input.namespace.clone());
    filters.push(input.name.clone());

    let guard = connexion.lock().await;
    let option = deployments::find_one_by_filters(&guard, filters);
    let config = option.as_ref().unwrap();

    // deployment found
    if config.is_some() {
        info!("Found deployment");
        let mut deployment = config.clone().unwrap();

        //@todo: implement reel deployment diff
        if input.image.clone() != deployment.image {
            info!("Image changed");
            println!("Image changed");

            deployment.status = "delete".to_string();
            deployments::update(&guard, &deployment);

            deployment.image = input.image.clone();
            deployments::create(&guard, &deployment);

            debug!("{:?}", deployment);
        }

        let deployment_output = hydrate_deployment_output(deployment);

        (StatusCode::CREATED, Json(deployment_output))

    }  else {
        info!("Deployment not found, create a new one");

        let utc: DateTime<Utc> = Utc::now();
        let deployment = deployments::Deployment {
            id: Uuid::new_v4().to_string(),
            name: input.name.clone(),
            runtime: input.runtime.clone(),
            namespace: input.namespace.clone(),
            image: input.image.clone(),
            status: "running".to_string(),
            created_at: utc.timestamp(),
            labels: input.labels,
            instances: [].to_vec(),
            replicas: input.replicas,
        };

        deployments::create(&guard, &deployment);

        let deployment_output = hydrate_deployment_output(deployment);

        return (StatusCode::CREATED, Json(deployment_output));
    }
}

async fn deployment_get(Path(id): Path<String>, Extension(connexion): Extension<Db>) -> impl IntoResponse {
    let guard = connexion.lock().await;

    let option = deployments::find(guard, id);

    let deployment = option.unwrap().unwrap();

    let instances = docker::list_instances(deployment.id.to_string()).await;

    let mut output = hydrate_deployment_output(deployment);
    output.instances = instances;

    Json(output)
}

pub(crate) async fn start(storage: Arc<Mutex<Connection>>, mut configuration: Config)
{
    debug!("Pre start http server");

    let connexion = Arc::clone(&storage);

    let app = Router::new()
        .route("/deployments", get(deployment_list).post(deployment_create))
        .route("/deployments/:id", get(deployment_get))

        .layer(
            ServiceBuilder::new()
                .layer(HandleErrorLayer::new(|error: BoxError| async move {
                    if error.is::<tower::timeout::error::Elapsed>() {
                        Ok(StatusCode::REQUEST_TIMEOUT)
                    } else {
                        Err((
                            StatusCode::INTERNAL_SERVER_ERROR,
                            format!("Unhandled internal error: {}", error),
                        ))
                    }
                }))
                .timeout(Duration::from_secs(10))
                .layer(Extension(connexion))
                .into_inner(),
        );

    let addr = SocketAddr::from(([0, 0, 0, 0], configuration.api.port));
    axum::Server::bind(&addr)
        .serve(app.into_make_service())
        .await
        .unwrap();

    info!("Starting server on {}", configuration.get_api_url());
}

#[derive(Deserialize, Serialize, Debug, Clone)]
struct DeploymentInput {
    name: String,
    runtime: String,
    namespace: String,
    image: String,
    replicas: i64,
    labels: String
}

#[derive(Deserialize, Serialize, Debug, Clone)]
struct DeploymentOutput {
    id: String,
    created_at: String,
    status: String,
    name: String,
    runtime: String,
    namespace: String,
    image: String,
    replicas: i64,
    ports: Vec<String>,
    labels: HashMap<String, String>,
    instances: Vec<String>
}

fn hydrate_deployment_output(deployment: deployments::Deployment) -> DeploymentOutput {
    let labels: HashMap<String, String> = deployments::Deployment::deserialize_labels(&deployment.labels);

    return DeploymentOutput {
        id: deployment.id,
        created_at: NaiveDateTime::from_timestamp(deployment.created_at, 0).to_string(),
        status: deployment.status,
        name: deployment.name,
        namespace: deployment.namespace,
        runtime: deployment.runtime,
        image: deployment.image,
        replicas: deployment.replicas,
        ports: [].to_vec(),
        labels: labels,
        instances: [].to_vec()
    };
}
