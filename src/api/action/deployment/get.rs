use axum::{
    extract::{Extension, Path},
    response::IntoResponse,
    Json
};

use crate::api::server::Db;
use crate::api::server::hydrate_deployment_output;
use crate::models::deployments;
use crate::runtime::docker;

pub(crate) async fn get(Path(id): Path<String>, Extension(connexion): Extension<Db>) -> impl IntoResponse {
    let guard = connexion.lock().await;

    let option = deployments::find(guard, id);

    let deployment = option.unwrap().unwrap();

    let instances = docker::list_instances(deployment.id.to_string()).await;

    let mut output = hydrate_deployment_output(deployment);
    output.instances = instances;

    Json(output)
}
