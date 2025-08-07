use axum::{
    extract::{Path, State},
    response::IntoResponse,
    Json
};

use crate::api::server::Db;
use crate::models::deployments;
use crate::runtime::runtime::Runtime;
use crate::models::users::User;

pub(crate) async fn logs(
    Path(id): Path<String>,
    _user: User,
    State(connexion): State<Db>,
) -> impl IntoResponse {
    let guard = connexion.lock().await;
    let deployment_result = deployments::find(&guard, id.clone());

    match deployment_result {
        Ok(Some(deployment)) => {
            let runtime = Runtime::new(deployment);
            let logs = runtime.get_logs().await;
            Json(logs)
        }
        Ok(None) => {
            Json(Vec::<crate::runtime::runtime::Log>::new())
        }
        Err(_) => {
            Json(Vec::<crate::runtime::runtime::Log>::new())
        }
    }
}