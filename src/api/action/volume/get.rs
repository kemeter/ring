use axum::Json;
use axum::extract::{Path, State};
use axum::response::IntoResponse;
use http::StatusCode;
use serde::Serialize;

use crate::api::auth::{Auth, require_namespace};
use crate::api::server::Db;
use crate::models::volumes;

#[derive(Serialize)]
struct VolumeOutput {
    id: String,
    created_at: String,
    updated_at: Option<String>,
    namespace: String,
    name: String,
    size: Option<i64>,
    backend_type: String,
    host_path: String,
    labels: std::collections::HashMap<String, String>,
}

pub(crate) async fn get(
    Path(id): Path<String>,
    State(pool): State<Db>,
    auth: Auth,
) -> impl IntoResponse {
    match volumes::find(&pool, &id).await {
        Ok(Some(volume)) => {
            // The namespace is only known once the row is loaded, so the
            // boundary is enforced here rather than by the scope middleware.
            // `host_path` in particular must not leak across namespaces.
            if let Err(response) = require_namespace(&auth.source, &volume.namespace) {
                return response;
            }

            let output = VolumeOutput {
                labels: volume.labels_map(),
                id: volume.id,
                created_at: volume.created_at,
                updated_at: volume.updated_at,
                namespace: volume.namespace,
                name: volume.name,
                size: volume.size,
                backend_type: volume.backend_type,
                host_path: volume.host_path,
            };
            (StatusCode::OK, Json(output)).into_response()
        }
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "Volume not found" })),
        )
            .into_response(),
        Err(e) => {
            error!("Failed to get volume: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": "Failed to get volume" })),
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
            .get("/volumes/00000000-0000-0000-0000-000000000000")
            .add_header("Authorization", format!("Bearer {}", token))
            .await;

        assert_eq!(response.status_code(), StatusCode::NOT_FOUND);
    }
}
