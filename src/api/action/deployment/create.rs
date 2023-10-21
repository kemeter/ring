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
use crate::api::dto::deployment::DeploymentOutput;
use crate::models::deployments::DeploymentConfig;
use crate::models::users::User;

fn default_replicas() -> u32 { 1 }


#[derive(Deserialize, Serialize, Debug, Clone)]
pub(crate) struct DeploymentInput {
    name: String,
    runtime: String,
    namespace: String,
    image: String,
    config: Option<DeploymentConfig>,
    #[serde(default = "default_replicas")]
    replicas: u32,
    #[serde(default)]
    labels: HashMap<String, String>,
    #[serde(default)]
    secrets: HashMap<String, String>,
    #[serde(default)]
    volumes: Vec<HashMap<String, String>>,
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
        info!("Found deployment");
        let mut deployment = config.clone().unwrap();

        //@todo: implement reel deployment diff
        if input.image.to_string() != deployment.image || query_parameters.force.is_some() {
            info!("force update");

            deployment.status = "deleted".to_string();
            deployments::update(&guard, &deployment);

            deployment.image = input.image.clone();
            deployment.id = Uuid::new_v4().to_string();
            deployment.labels = input.labels;
            deployment.secrets = input.secrets;
            deployments::create(&guard, &deployment);
        }

        let deployment_output = DeploymentOutput::from_to_model(deployment);

        (StatusCode::CREATED, Json(deployment_output))

    }  else {
        info!("Deployment not found, create a new one");

        let utc: DateTime<Utc> = Utc::now();
        let volumes = serde_json::to_string(&input.volumes).unwrap();

        let deployment = deployments::Deployment {
            id: Uuid::new_v4().to_string(),
            name: input.name.clone(),
            runtime: input.runtime.clone(),
            namespace: input.namespace.clone(),
            kind: String::from("worker"),
            image: input.image.clone(),
            config: input.config.clone(),
            status: "running".to_string(),
            created_at: utc.to_string(),
            labels: input.labels,
            secrets: input.secrets,
            replicas: input.replicas,
            instances: [].to_vec(),
            volumes: volumes
        };

        deployments::create(&guard, &deployment);

        let deployment_output = DeploymentOutput::from_to_model(deployment);

        return (StatusCode::CREATED, Json(deployment_output));
    }
}
