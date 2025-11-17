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
    State(connexion): State<Db>,
    _user: User,
) -> impl IntoResponse {
    let guard = connexion.lock().await;
    
    // First verify the deployment exists
    match deployments::find(&guard, deployment_id.clone()) {
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

    // Get health check results
    let results = if params.latest.unwrap_or(false) {
        // Get latest results for each check type
        match health_check_logs::find_latest_by_deployment(&guard, deployment_id) {
            Ok(results) => results,
            Err(e) => {
                let message = Message { message: format!("Failed to fetch health check results: {}", e) };
                return (StatusCode::INTERNAL_SERVER_ERROR, Json(message)).into_response();
            }
        }
    } else {
        // Get all results with limit
        match health_check_logs::find_by_deployment(&guard, deployment_id, params.limit) {
            Ok(results) => results,
            Err(e) => {
                let message = Message { message: format!("Failed to fetch health check results: {}", e) };
                return (StatusCode::INTERNAL_SERVER_ERROR, Json(message)).into_response();
            }
        }
    };

    (StatusCode::OK, Json(results)).into_response()
}

