use axum::{
    extract::{Path},
    response::IntoResponse,
    Json
};
use axum::extract::State;

use crate::api::server::Db;
use crate::models::deployments;
use crate::api::dto::deployment::DeploymentOutput;
use crate::runtime::docker;

pub(crate) async fn get(
    Path(id): Path<String>,
    State(connexion): State<Db>,
) -> impl IntoResponse {
    let guard = connexion.lock().await;

    let option = deployments::find(&guard, id.clone());

    let deployment = option.unwrap().unwrap();

    let instances = docker::list_instances(id).await;

    let mut output = DeploymentOutput::from_to_model(deployment);
    output.instances = instances;

    Json(output)
}
