use chrono::{DateTime, Utc};
use uuid::Uuid;

use axum::{
    extract::{Extension},
    http::StatusCode,
    response::IntoResponse,
    Json
};

use serde::{Serialize, Deserialize};
use std::collections::HashMap;
use axum::extract::Query;

use crate::api::server::Db;
use crate::models::deployments;
use crate::api::dto::deployment::hydrate_deployment_output;
use crate::models::users::User;

#[derive(Deserialize, Serialize, Debug, Clone)]
pub(crate) struct DeploymentInput {
    name: String,
    runtime: String,
    namespace: String,
    image: String,
    replicas: u32,
    labels: String,
    secrets: String,
    volumes: String
}

#[derive(Deserialize, Debug)]
pub(crate) struct QueryParameters {
    force: Option<bool>
}

pub(crate) async fn create(
    query_parameters: Query<QueryParameters>,
    Json(input): Json<DeploymentInput>,
    Extension(connexion): Extension<Db>, _user: User
) -> impl IntoResponse {
    let mut filters = Vec::new();
    filters.push(input.namespace.clone());
    filters.push(input.name.clone());

    let guard = connexion.lock().await;
    let option = deployments::find_one_by_filters(&guard, filters);
    let config = option.as_ref().unwrap();

    // deployment found
    if config.is_some() {
        info!("Found deployment", );
        let mut deployment = config.clone().unwrap();

        //@todo: implement reel deployment diff
        if input.image.to_string() != deployment.image || query_parameters.force.is_some() {
            info!("force update");

            deployment.status = "deleted".to_string();
            deployments::update(&guard, &deployment);

            deployment.image = input.image.clone();
            deployment.id = Uuid::new_v4().to_string();
            deployments::create(&guard, &deployment);
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
            created_at: utc.to_string(),
            labels: input.labels,
            secrets: input.secrets,
            replicas: input.replicas,
            instances: [].to_vec(),
            volumes: input.volumes
        };

        deployments::create(&guard, &deployment);

        let deployment_output = hydrate_deployment_output(deployment);

        return (StatusCode::CREATED, Json(deployment_output));
    }
}
