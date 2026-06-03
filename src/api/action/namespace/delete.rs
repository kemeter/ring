use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde_json::json;

use crate::api::auth::Auth;
use crate::api::server::Db;
use crate::models::audit_log;
use crate::models::namespace;

/// `DELETE /namespaces/{name}` — remove an empty namespace and purge its
/// audit trail (retention is namespace-bound by design).
///
/// Refuses with 409 if the namespace still holds live deployments, secrets
/// or configs: we never cascade-delete an operator's resources.
// The path param is `{id}` to share the route shape with `namespace_get`,
// but namespaces are addressed by name throughout the API, so we treat it
// as a name (find_by_name / delete_by_name).
pub(crate) async fn delete(
    Path(name): Path<String>,
    State(pool): State<Db>,
    auth: Auth,
) -> impl IntoResponse {
    // Scope (`namespaces:write`) is enforced centrally by the auth middleware.
    match namespace::find_by_name(&pool, &name).await {
        Ok(Some(_)) => {}
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            log::error!("Failed to look up namespace '{}': {}", name, e);
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }

    match namespace::count_resources(&pool, &name).await {
        Ok(0) => {}
        Ok(n) => {
            return (
                StatusCode::CONFLICT,
                Json(json!({
                    "error": "namespace is not empty",
                    "remaining": n,
                    "hint": "delete its deployments, secrets and configs first"
                })),
            )
                .into_response();
        }
        Err(e) => {
            log::error!("Failed to count resources in namespace '{}': {}", name, e);
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }

    if let Err(e) = namespace::delete_by_name(&pool, &name).await {
        log::error!("Failed to delete namespace '{}': {}", name, e);
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    // Purge the namespace's audit trail, then record the deletion itself.
    // The delete entry is written under the (now gone) namespace name so it
    // is the last thing visible if the trail is consulted before retention
    // cleanup elsewhere — but since we just purged, this is intentionally
    // the sole surviving entry for the deleted namespace.
    let _ = audit_log::delete_by_namespace(&pool, &name).await;
    let _ = audit_log::record(
        &pool,
        Some(&auth.user.id),
        "delete",
        "namespace",
        &name,
        Some(&name),
    )
    .await;

    StatusCode::NO_CONTENT.into_response()
}

#[cfg(test)]
mod tests {
    use crate::api::server::tests::{login, new_test_app, new_test_app_with_pool};
    use crate::models::audit_log;
    use axum::http::StatusCode;
    use axum_test::TestServer;
    use serde_json::json;

    #[tokio::test]
    async fn delete_empty_namespace_succeeds() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        server
            .post("/namespaces")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({ "name": "throwaway" }))
            .await;

        let response = server
            .delete("/namespaces/throwaway")
            .add_header("Authorization", format!("Bearer {}", token))
            .await;
        assert_eq!(response.status_code(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn delete_unknown_namespace_is_404() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response = server
            .delete("/namespaces/nope")
            .add_header("Authorization", format!("Bearer {}", token))
            .await;
        assert_eq!(response.status_code(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn delete_non_empty_namespace_is_409() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        server
            .post("/namespaces")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({ "name": "busy" }))
            .await;
        // A config makes the namespace non-empty.
        server
            .post("/configs")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({ "namespace": "busy", "name": "c1", "data": "{}" }))
            .await;

        let response = server
            .delete("/namespaces/busy")
            .add_header("Authorization", format!("Bearer {}", token))
            .await;
        assert_eq!(response.status_code(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn deleting_namespace_purges_its_audit_trail() {
        let (pool, app) = new_test_app_with_pool().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        server
            .post("/namespaces")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({ "name": "ephemeral" }))
            .await;

        // Trail has the create entry.
        assert_eq!(
            audit_log::find_by_namespace(&pool, "ephemeral", None)
                .await
                .unwrap()
                .len(),
            1
        );

        let response = server
            .delete("/namespaces/ephemeral")
            .add_header("Authorization", format!("Bearer {}", token))
            .await;
        assert_eq!(response.status_code(), StatusCode::NO_CONTENT);

        // Old trail purged; only the delete entry itself remains.
        let after = audit_log::find_by_namespace(&pool, "ephemeral", None)
            .await
            .unwrap();
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].action, "delete");
        assert_eq!(after[0].target_type, "namespace");
    }

    #[tokio::test]
    async fn delete_requires_authentication() {
        let app = new_test_app().await;
        let server = TestServer::new(app).unwrap();
        let response = server.delete("/namespaces/x").await;
        assert_eq!(response.status_code(), StatusCode::UNAUTHORIZED);
    }
}
