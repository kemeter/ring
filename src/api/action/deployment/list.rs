use std::collections::HashMap;
use axum::{
    extract::{Query},
    response::IntoResponse,
    Json,
};
use axum::extract::State;
use serde::Deserialize;

use crate::api::server::Db;
use crate::api::dto::deployment::DeploymentOutput;
use crate::models::deployments;
use crate::runtime::docker;
use crate::models::users::User;

#[derive(Deserialize, Debug)]
pub(crate) struct QueryParameters {
    namespace: Option<String>
}

pub(crate) async fn list(
    query_parameters: Query<QueryParameters>,
    State(connexion): State<Db>,
    _user: User
) -> impl IntoResponse {

    let mut deployments: Vec<DeploymentOutput> = Vec::new();

    let list_deployments = {
        let guard = connexion.lock().await;
        let mut filters = HashMap::new();

        if query_parameters.namespace.is_some() {
            filters.insert(String::from("namespace"), query_parameters.namespace.clone().unwrap().to_string());
        }

        deployments::find_all(&guard, filters)
    };

    for deployment in list_deployments.into_iter() {

        let mut output = DeploymentOutput::from_to_model(deployment.clone());
        let instances = docker::list_instances(deployment.id.to_string()).await;
        output.instances = instances;

        deployments.push(output);
    }

    Json(deployments)
}
