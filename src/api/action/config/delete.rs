use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use http::StatusCode;

use crate::api::auth::{Auth, require_namespace};
use crate::api::server::Db;
use crate::models::audit_log;
use crate::models::config as ConfigModel;

pub(crate) async fn delete(Path(id): Path<String>, State(pool): State<Db>, auth: Auth) -> Response {
    // Load the row before deleting so the audit entry carries the real
    // namespace and name (same pattern as deployment/secret delete).
    let config = match ConfigModel::find(&pool, &id).await {
        Ok(Some(c)) => c,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(err) => {
            error!("Failed to look up configuration {}: {}", id, err);
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    // Scope (`configs:write`) is enforced centrally; the namespace boundary is
    // checked here against the loaded config.
    if let Err(resp) = require_namespace(&auth.source, &config.namespace) {
        return resp;
    }

    let result = ConfigModel::delete(&pool, &id).await;
    if let Err(ref err) = result {
        error!("Failed to delete configuration with ID {}: {}", id, err);
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    let _ = audit_log::record(
        &pool,
        Some(&auth.user.id),
        "delete",
        "config",
        &config.name,
        Some(&config.namespace),
    )
    .await;

    StatusCode::NO_CONTENT.into_response()
}

#[cfg(test)]
mod tests {
    use crate::api::server::tests::{login, new_test_app};
    use axum::http::StatusCode;
    use axum_test::TestServer;

    #[tokio::test]
    async fn delete() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response = server
            .delete("/configs/cde7806a-21af-473b-968b-08addc7bf0ba")
            .add_header("Authorization", format!("Bearer {}", token))
            .await;

        assert_eq!(response.status_code(), StatusCode::NO_CONTENT);

        let response = server
            .get("/configs/cde7806a-21af-473b-968b-08addc7bf0ba")
            .add_header("Authorization", format!("Bearer {}", token))
            .await;

        assert_eq!(response.status_code(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn delete_not_found() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response = server
            .delete("/configs/00000000-0000-0000-0000-000000000000")
            .add_header("Authorization", format!("Bearer {}", token))
            .await;

        assert_eq!(response.status_code(), StatusCode::NOT_FOUND);
    }
}
