use chrono::{DateTime, Utc};
use uuid::Uuid;

use axum::{
    extract::{Extension},
    http::StatusCode,
    response::IntoResponse,
    Json
};

use serde::{Serialize, Deserialize};

use crate::api::server::hydrate_deployment_output;
use crate::api::server::Db;
use crate::models::deployments;

#[derive(Deserialize, Serialize, Debug, Clone)]
pub(crate) struct DeploymentInput {
    name: String,
    runtime: String,
    namespace: String,
    image: String,
    replicas: i64,
    labels: String
}

pub(crate) async fn create(Json(input): Json<DeploymentInput>, Extension(connexion): Extension<Db>) -> impl IntoResponse {
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
