use axum::extract::State;
use axum::Json;
use axum::response::IntoResponse;
use axum::http::StatusCode;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use crate::api::server::Db;
use crate::models::secret;
use crate::models::users::User;

#[derive(Deserialize, Serialize, Debug, Clone)]
pub(crate) struct SecretInput {
    namespace: String,
    name: String,
    value: String,
}

#[derive(Serialize)]
struct SecretOutput {
    id: String,
    created_at: String,
    namespace: String,
    name: String,
}

pub(crate) async fn create(
    State(pool): State<Db>,
    _user: User,
    Json(input): Json<SecretInput>,
) -> impl IntoResponse {
    let encrypted_value = secret::encrypt_value(&input.value);

    let new_secret = secret::Secret {
        id: Uuid::new_v4().to_string(),
        created_at: Utc::now().to_string(),
        updated_at: None,
        namespace: input.namespace,
        name: input.name,
        value: encrypted_value,
    };

    match secret::create(&pool, &new_secret).await {
        Ok(_) => {
            let output = SecretOutput {
                id: new_secret.id,
                created_at: new_secret.created_at,
                namespace: new_secret.namespace,
                name: new_secret.name,
            };
            (StatusCode::CREATED, Json(output)).into_response()
        }
        Err(e) => {
            if e.to_string().contains("UNIQUE constraint failed") {
                (StatusCode::CONFLICT, Json(serde_json::json!({
                    "error": "Secret with this name already exists in this namespace"
                }))).into_response()
            } else {
                log::error!("Failed to create secret: {}", e);
                (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({
                    "error": "Failed to create secret"
                }))).into_response()
            }
        }
    }
}
