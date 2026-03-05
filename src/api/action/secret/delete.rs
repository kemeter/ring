use axum::extract::{Path, State};
use axum::response::IntoResponse;
use axum::Json;
use http::StatusCode;

use crate::api::server::Db;
use crate::models::secret as SecretModel;
use crate::models::users::User;

pub(crate) async fn delete(
    Path(id): Path<String>,
    State(pool): State<Db>,
    _user: User,
) -> impl IntoResponse {
    match SecretModel::delete(&pool, &id).await {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(sqlx::Error::RowNotFound) => {
            (StatusCode::NOT_FOUND, Json(serde_json::json!({
                "error": "Secret not found"
            }))).into_response()
        }
        Err(e) => {
            log::error!("Failed to delete secret with ID {}: {}", id, e);
            (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({
                "error": "Failed to delete secret"
            }))).into_response()
        }
    }
}
