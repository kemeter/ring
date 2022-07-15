use axum::{
    extract::{Extension},
    response::IntoResponse,
    Json,
};

use crate::api::server::Db;
use crate::api::dto::deployment::DeploymentOutput;
use crate::models::deployments;
use crate::runtime::docker;
use crate::models::users::User;
use crate::api::dto::deployment::hydrate_deployment_output;

pub(crate) async fn list(Extension(connexion): Extension<Db>, _user: User) -> impl IntoResponse {

    let mut deployments: Vec<DeploymentOutput> = Vec::new();

    let list_deployments = {
        let guard = connexion.lock().await;
        deployments::find_all(guard)
    };

    for deployment in list_deployments.into_iter() {
        let d = deployment.clone();

        let mut output = hydrate_deployment_output(deployment);
        let instances = docker::list_instances(d.id.to_string()).await;
        output.instances = instances;

        deployments.push(output);
    }

    Json(deployments)
}
