use axum::Json;
use axum::extract::{Path, State};
use axum::response::IntoResponse;
use http::StatusCode;
use serde::Serialize;

use crate::api::auth::{Auth, require_namespace};
use crate::api::server::Db;
use crate::models::secret as SecretModel;

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
    auth: Auth,
) -> impl IntoResponse {
    match SecretModel::find(&pool, &id).await {
        Ok(Some(secret)) => {
            // Scope (`secrets:read`) is enforced centrally; the namespace
            // boundary can only be checked now that the secret is loaded.
            if let Err(resp) = require_namespace(&auth.source, &secret.namespace) {
                return resp;
            }
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
            error!("Failed to get secret: {}", e);
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
    use crate::api::testing::TestApp;

    use crate::api::server::tests::{login, new_test_app};
    use axum::http::StatusCode;
    use axum_test::TestServer;

    #[tokio::test]
    async fn get_not_found() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app);

        let response = server
            .get("/secrets/00000000-0000-0000-0000-000000000000")
            .add_header("Authorization", format!("Bearer {}", token))
            .await;

        assert_eq!(response.status_code(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn pat_limited_to_another_namespace_cannot_read_it() {
        crate::models::secret::set_test_key();
        let mut app = TestApp::admin().await;
        app.namespace("tenant-b").await;
        let id = app
            .create(
                "/secrets",
                serde_json::json!({ "namespace": "tenant-b", "name": "db", "value": "hunter2" }),
            )
            .await;
        app.act_as_pat(&["secrets:read"], &["tenant-a"]).await;
        app.get(&format!("/secrets/{id}"))
            .await
            .assert_status_forbidden();
    }
}
