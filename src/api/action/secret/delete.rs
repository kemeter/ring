use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use axum::Json;
use http::StatusCode;
use serde::Deserialize;

use crate::api::server::Db;
use crate::models::secret as SecretModel;
use crate::models::deployments;
use crate::models::users::User;

#[derive(Deserialize)]
pub(crate) struct DeleteQuery {
    #[serde(default)]
    force: bool,
}

pub(crate) async fn delete(
    Path(id): Path<String>,
    Query(query): Query<DeleteQuery>,
    State(pool): State<Db>,
    _user: User,
) -> impl IntoResponse {
    // First, find the secret to get namespace and name
    let secret = match SecretModel::find(&pool, &id).await {
        Ok(Some(s)) => s,
        Ok(None) => {
            return (StatusCode::NOT_FOUND, Json(serde_json::json!({
                "error": "Secret not found"
            }))).into_response();
        }
        Err(e) => {
            log::error!("Failed to find secret {}: {}", id, e);
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({
                "error": "Failed to find secret"
            }))).into_response();
        }
    };

    // Check for deployments referencing this secret
    let referencing = match deployments::find_referencing_secret(&pool, &secret.namespace, &secret.name).await {
        Ok(deps) => deps,
        Err(e) => {
            log::error!("Failed to check secret references: {}", e);
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({
                "error": "Failed to check references"
            }))).into_response();
        }
    };

    if !referencing.is_empty() && !query.force {
        let deployment_names: Vec<String> = referencing
            .iter()
            .map(|d| format!("{}/{}", d.namespace, d.name))
            .collect();

        return (StatusCode::CONFLICT, Json(serde_json::json!({
            "error": "Secret is referenced by deployments",
            "deployments": deployment_names,
            "hint": "Use ?force=true to delete anyway"
        }))).into_response();
    }

    match SecretModel::delete(&pool, &id).await {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => {
            log::error!("Failed to delete secret with ID {}: {}", id, e);
            (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({
                "error": "Failed to delete secret"
            }))).into_response()
        }
    }
}
