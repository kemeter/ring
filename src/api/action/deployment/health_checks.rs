use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};

use crate::api::auth::{Auth, require_namespace};
use crate::api::server::Db;
use crate::models::{deployments, health_check_logs};

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
    auth: Auth,
) -> impl IntoResponse {
    // Scope (`deployments:read`) is enforced centrally; the namespace boundary
    // is checked here against the loaded deployment.
    match deployments::find(&pool, &deployment_id).await {
        Ok(Some(deployment)) => {
            if let Err(resp) = require_namespace(&auth.source, &deployment.namespace) {
                return resp;
            }
        }
        Ok(None) => {
            let message = Message {
                message: "Deployment not found".to_string(),
            };
            return (StatusCode::NOT_FOUND, Json(message)).into_response();
        }
        Err(e) => {
            error!("Database error while fetching deployment: {}", e);
            let message = Message {
                message: "Internal server error".to_string(),
            };
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(message)).into_response();
        }
    }

    let results = if params.latest.unwrap_or(false) {
        match health_check_logs::find_latest_by_deployment(&pool, deployment_id).await {
            Ok(results) => results,
            Err(e) => {
                error!("Failed to fetch health check results: {}", e);
                let message = Message {
                    message: "Internal server error".to_string(),
                };
                return (StatusCode::INTERNAL_SERVER_ERROR, Json(message)).into_response();
            }
        }
    } else {
        match health_check_logs::find_by_deployment(&pool, deployment_id, params.limit).await {
            Ok(results) => results,
            Err(e) => {
                error!("Failed to fetch health check results: {}", e);
                let message = Message {
                    message: "Internal server error".to_string(),
                };
                return (StatusCode::INTERNAL_SERVER_ERROR, Json(message)).into_response();
            }
        }
    };

    (StatusCode::OK, Json(results)).into_response()
}
