use axum::{
    extract::{Path},
    response::IntoResponse,
    Json
};
use axum::extract::State;

use crate::api::server::Db;
use crate::models::deployments;
use crate::api::dto::deployment::DeploymentOutput;
use crate::runtime::runtime::Runtime;
use crate::models::users::User;

pub(crate) async fn get(
    Path(id): Path<String>,
    _user: User,
    State(connexion): State<Db>,
) -> impl IntoResponse {
    let guard = connexion.lock().await;

    let option = deployments::find(&guard, id.clone());
    let deployment = option.unwrap().unwrap();

    let runtime = Runtime::new(deployment.clone());
    let instances = runtime.list_instances().await;

    let mut output = DeploymentOutput::from_to_model(deployment);
    output.instances = instances;

    Json(output)
}
