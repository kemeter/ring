use axum::Json;
use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use http::StatusCode;
use serde::Deserialize;

use crate::api::auth::{Auth, require_namespace};
use crate::api::server::Db;
use crate::models::audit_log;
use crate::models::deployments;
use crate::models::secret as SecretModel;

#[derive(Deserialize)]
pub(crate) struct DeleteQuery {
    #[serde(default)]
    force: bool,
}

pub(crate) async fn delete(
    Path(id): Path<String>,
    Query(query): Query<DeleteQuery>,
    State(pool): State<Db>,
    auth: Auth,
) -> impl IntoResponse {
    // First, find the secret to get namespace and name
    let secret = match SecretModel::find(&pool, &id).await {
        Ok(Some(s)) => s,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({
                    "error": "Secret not found"
                })),
            )
                .into_response();
        }
        Err(e) => {
            error!("Failed to find secret {}: {}", id, e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": "Failed to find secret"
                })),
            )
                .into_response();
        }
    };

    // Scope (`secrets:write`) is enforced centrally; the namespace boundary is
    // checked here, now that the secret's real namespace is known.
    if let Err(resp) = require_namespace(&auth.source, &secret.namespace) {
        return resp.into_response();
    }

    // Check for deployments referencing this secret
    let referencing =
        match deployments::find_referencing_secret(&pool, &secret.namespace, &secret.name).await {
            Ok(deps) => deps,
            Err(e) => {
                error!("Failed to check secret references: {}", e);
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({
                        "error": "Failed to check references"
                    })),
                )
                    .into_response();
            }
        };

    if !referencing.is_empty() && !query.force {
        let deployment_names: Vec<String> = referencing
            .iter()
            .map(|d| format!("{}/{}", d.namespace, d.name))
            .collect();

        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "Secret is referenced by deployments",
                "deployments": deployment_names,
                "hint": "Use ?force=true to delete anyway"
            })),
        )
            .into_response();
    }

    match SecretModel::delete(&pool, &id).await {
        Ok(_) => {
            let _ = audit_log::record(
                &pool,
                Some(&auth.user.id),
                "delete",
                "secret",
                &secret.name,
                Some(&secret.namespace),
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => {
            error!("Failed to delete secret with ID {}: {}", id, e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": "Failed to delete secret"
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
    use serde_json::json;

    fn set_test_key() {
        use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
        let key = [0u8; 32];
        let key_b64 = BASE64.encode(key);
        unsafe { std::env::set_var("RING_SECRET_KEY", key_b64) };
    }

    #[tokio::test]
    async fn delete_not_found() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app);

        let response = server
            .delete("/secrets/00000000-0000-0000-0000-000000000000")
            .add_header("Authorization", format!("Bearer {}", token))
            .await;

        assert_eq!(response.status_code(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn delete_secret_success() {
        set_test_key();
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app);

        // Create a namespace first
        server
            .post("/namespaces")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({"name": "test-delete"}))
            .await;

        // Create a secret
        let create_response = server
            .post("/secrets")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "namespace": "test-delete",
                "name": "my-secret",
                "value": "secret-value"
            }))
            .await;

        assert_eq!(create_response.status_code(), StatusCode::CREATED);
        let secret: serde_json::Value = create_response.json();
        let secret_id = secret["id"].as_str().unwrap();

        // Delete it
        let response = server
            .delete(&format!("/secrets/{}", secret_id))
            .add_header("Authorization", format!("Bearer {}", token))
            .await;

        assert_eq!(response.status_code(), StatusCode::NO_CONTENT);
    }

    /// Creates the namespace `refs`, the secrets `db-pass` and `db-pass-old`,
    /// and a live deployment `web` built by `deployment` over a base that runs
    /// in `refs`. Returns the ids of both secrets.
    async fn setup_referenced_secret(
        server: &TestServer,
        token: &str,
        deployment: serde_json::Value,
    ) -> (String, String) {
        set_test_key();
        server
            .post("/namespaces")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({ "name": "refs" }))
            .await;

        let mut ids = Vec::new();
        for name in ["db-pass", "db-pass-old"] {
            let response = server
                .post("/secrets")
                .add_header("Authorization", format!("Bearer {}", token))
                .json(&json!({ "namespace": "refs", "name": name, "value": "{}" }))
                .await;
            assert_eq!(response.status_code(), StatusCode::CREATED);
            let secret: serde_json::Value = response.json();
            ids.push(secret["id"].as_str().unwrap().to_string());
        }

        let mut body = json!({
            "runtime": "docker",
            "name": "web",
            "namespace": "refs",
            "image": "nginx:latest",
        });
        body.as_object_mut()
            .unwrap()
            .extend(deployment.as_object().unwrap().clone());
        let response = server
            .post("/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&body)
            .await;
        assert_eq!(
            response.status_code(),
            StatusCode::CREATED,
            "{}",
            response.text()
        );

        (ids[0].clone(), ids[1].clone())
    }

    async fn assert_delete_is_blocked(deployment: serde_json::Value) {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app);
        let (used, unused) = setup_referenced_secret(&server, &token, deployment).await;

        let response = server
            .delete(&format!("/secrets/{}", used))
            .add_header("Authorization", format!("Bearer {}", token))
            .await;
        assert_eq!(response.status_code(), StatusCode::CONFLICT);
        let body: serde_json::Value = response.json();
        assert_eq!(body["deployments"], json!(["refs/web"]));

        // A secret whose name only starts like the used one is not in use.
        let response = server
            .delete(&format!("/secrets/{}", unused))
            .add_header("Authorization", format!("Bearer {}", token))
            .await;
        assert_eq!(response.status_code(), StatusCode::NO_CONTENT);

        let response = server
            .delete(&format!("/secrets/{}?force=true", used))
            .add_header("Authorization", format!("Bearer {}", token))
            .await;
        assert_eq!(response.status_code(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn a_secret_used_in_the_environment_cannot_be_deleted_without_force() {
        assert_delete_is_blocked(json!({
            "environment": { "DB_PASSWORD": { "secretRef": "db-pass" } }
        }))
        .await;
    }

    #[tokio::test]
    async fn a_secret_mounted_as_a_volume_cannot_be_deleted_without_force() {
        assert_delete_is_blocked(json!({
            "volumes": [{
                "type": "secret",
                "source": "db-pass",
                "destination": "/run/secrets/db-pass",
                "driver": "local",
                "permission": "ro"
            }]
        }))
        .await;
    }

    #[tokio::test]
    async fn an_image_pull_secret_cannot_be_deleted_without_force() {
        assert_delete_is_blocked(json!({
            "config": { "image_pull_secret": "db-pass" }
        }))
        .await;
    }
}
