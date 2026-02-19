use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};

use crate::api::server::Db;
use crate::models::{health_check_logs, deployments};
use crate::models::users::User;

#[derive(Deserialize)]
pub(crate) struct HealthCheckQuery {
    limit: Option<u32>,
    latest: Option<bool>,
}

#[derive(Serialize, Deserialize, Debug)]
struct Message {
    message: String,
}

pub(crate) async fn get_health_checks(
    Path(deployment_id): Path<String>,
    Query(params): Query<HealthCheckQuery>,
    State(pool): State<Db>,
    _user: User,
) -> impl IntoResponse {
    match deployments::find(&pool, deployment_id.clone()).await {
        Ok(Some(_)) => {},
        Ok(None) => {
            let message = Message { message: "Deployment not found".to_string() };
            return (StatusCode::NOT_FOUND, Json(message)).into_response();
        },
        Err(e) => {
            let message = Message { message: format!("Database error: {}", e) };
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(message)).into_response();
        }
    }

    let results = if params.latest.unwrap_or(false) {
        match health_check_logs::find_latest_by_deployment(&pool, deployment_id).await {
            Ok(results) => results,
            Err(e) => {
                let message = Message { message: format!("Failed to fetch health check results: {}", e) };
                return (StatusCode::INTERNAL_SERVER_ERROR, Json(message)).into_response();
            }
        }
    } else {
        match health_check_logs::find_by_deployment(&pool, deployment_id, params.limit).await {
            Ok(results) => results,
            Err(e) => {
                let message = Message { message: format!("Failed to fetch health check results: {}", e) };
                return (StatusCode::INTERNAL_SERVER_ERROR, Json(message)).into_response();
            }
        }
    };

    (StatusCode::OK, Json(results)).into_response()
}
