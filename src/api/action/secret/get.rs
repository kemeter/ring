use axum::Json;
use axum::extract::{Path, State};
use axum::response::IntoResponse;
use http::StatusCode;
use serde::Serialize;

use crate::api::server::Db;
use crate::models::secret as SecretModel;
use crate::models::users::User;

#[derive(Serialize)]
struct SecretOutput {
    id: String,
    created_at: String,
    updated_at: Option<String>,
    namespace: String,
    name: String,
}

pub(crate) async fn get(
    Path(id): Path<String>,
    State(pool): State<Db>,
    _user: User,
) -> impl IntoResponse {
    match SecretModel::find(&pool, &id).await {
        Ok(Some(secret)) => {
            let output = SecretOutput {
                id: secret.id,
                created_at: secret.created_at,
                updated_at: secret.updated_at,
                namespace: secret.namespace,
                name: secret.name,
            };
            (StatusCode::OK, Json(output)).into_response()
        }
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": "Secret not found"
            })),
        )
            .into_response(),
        Err(e) => {
            log::error!("Failed to get secret: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": "Failed to get secret"
                })),
            )
                .into_response()
        }
    }
}
