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

#[cfg(test)]
mod tests {
    use crate::api::server::tests::{login, new_test_app};
    use axum::http::StatusCode;
    use axum_test::TestServer;

    #[tokio::test]
    async fn get_not_found() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response = server
            .get("/secrets/00000000-0000-0000-0000-000000000000")
            .add_header("Authorization", format!("Bearer {}", token))
            .await;

        assert_eq!(response.status_code(), StatusCode::NOT_FOUND);
    }
}
