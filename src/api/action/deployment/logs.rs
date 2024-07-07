use axum::{
    extract::{Path, State},
    response::IntoResponse,
    Json
};

use crate::api::server::Db;
use crate::models::deployments;
use crate::runtime::runtime::Runtime;

pub(crate) async fn logs(
    Path(id): Path<String>,
    State(connexion): State<Db>,
) -> impl IntoResponse {
    let guard = connexion.lock().await;
    let option = deployments::find(&guard, id.clone());

    let deployment = option.unwrap().unwrap();

    let runtime = Runtime::new(deployment.clone());
    let logs = runtime.get_logs().await;

    Json(logs)
}