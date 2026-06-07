use axum::extract::{Path, Query, State};
use axum::response::Response;
use axum::{Json, response::IntoResponse};
use serde::Deserialize;

use crate::api::dto::audit::AuditOutput;
use crate::api::server::Db;
use crate::models::audit_log;

#[derive(Debug, Deserialize)]
pub(crate) struct AuditQuery {
    #[serde(default)]
    limit: Option<u32>,
}

/// `GET /namespaces/{name}/audit` — the write-action trail for a namespace,
/// most recent first. Returns an empty list for an unknown namespace (the
/// trail is keyed by name, not by a namespace row, so it stays readable even
/// after the namespace itself is gone within the retention window).
// Scope (`namespaces:read`) is enforced centrally by the auth middleware.
pub(crate) async fn audit(
    Path(name): Path<String>,
    Query(params): Query<AuditQuery>,
    State(pool): State<Db>,
) -> Response {
    let entries = match audit_log::find_by_namespace(&pool, &name, params.limit).await {
        Ok(entries) => entries,
        Err(e) => {
            error!("Failed to read audit log for namespace '{}': {}", name, e);
            return Json(Vec::<AuditOutput>::new()).into_response();
        }
    };

    Json(
        entries
            .into_iter()
            .map(AuditOutput::from_to_model)
            .collect::<Vec<_>>(),
    )
    .into_response()
}

#[cfg(test)]
mod tests {
    use crate::api::dto::audit::AuditOutput;
    use crate::api::server::tests::{login, new_test_app};
    use axum::http::StatusCode;
    use axum_test::TestServer;
    use serde_json::json;

    #[tokio::test]
    async fn audit_lists_namespace_write_actions() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        // Two write actions in the same namespace.
        server
            .post("/namespaces")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({ "name": "prod" }))
            .await;
        server
            .post("/configs")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "namespace": "prod",
                "name": "nginx-conf",
                "data": "{}"
            }))
            .await;

        let response = server
            .get("/namespaces/prod/audit")
            .add_header("Authorization", format!("Bearer {}", token))
            .await;

        assert_eq!(response.status_code(), StatusCode::OK);
        let entries = response.json::<Vec<AuditOutput>>();
        assert_eq!(entries.len(), 2);
        // Most recent first: the config create, then the namespace create.
        assert_eq!(entries[0].target_type, "config");
        assert_eq!(entries[0].action, "create");
        assert_eq!(entries[1].target_type, "namespace");
    }

    #[tokio::test]
    async fn audit_requires_authentication() {
        let app = new_test_app().await;
        let server = TestServer::new(app).unwrap();
        let response = server.get("/namespaces/prod/audit").await;
        assert_eq!(response.status_code(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn audit_unknown_namespace_is_empty() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response = server
            .get("/namespaces/does-not-exist/audit")
            .add_header("Authorization", format!("Bearer {}", token))
            .await;

        assert_eq!(response.status_code(), StatusCode::OK);
        assert!(response.json::<Vec<AuditOutput>>().is_empty());
    }
}
