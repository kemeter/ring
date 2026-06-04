use crate::api::auth::Auth;
use crate::api::server::Db;
use crate::api::validation::problem_response;
use crate::models::audit_log;
use crate::models::webhook;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

pub(crate) async fn delete(Path(id): Path<String>, State(pool): State<Db>, auth: Auth) -> Response {
    // Scope (`webhooks:write`) is enforced centrally by the auth middleware.
    let existing = match webhook::find(&pool, &id).await {
        Ok(Some(w)) => w,
        Ok(None) => {
            return problem_response(StatusCode::NOT_FOUND, "Not Found", "webhook not found");
        }
        Err(e) => {
            log::error!("Failed to look up webhook: {}", e);
            return problem_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Internal Server Error",
                "failed to delete webhook",
            );
        }
    };

    match webhook::revoke(&pool, &existing.id).await {
        Ok(_) => {
            let _ = audit_log::record(
                &pool,
                Some(&auth.user.id),
                "delete",
                "webhook",
                &existing.url,
                None,
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => {
            log::error!("Failed to delete webhook: {}", e);
            problem_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Internal Server Error",
                "failed to delete webhook",
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::api::server::tests::new_test_app_with_pool;
    use crate::models::token;
    use crate::models::webhook;
    use axum_test::TestServer;
    use http::StatusCode;

    const ADMIN_ID: &str = "1c5a5fe9-84e0-4a18-821e-8058232c2c23";

    async fn pat(pool: &sqlx::SqlitePool, scopes: &[&str]) -> String {
        let scopes: Vec<String> = scopes.iter().map(|s| s.to_string()).collect();
        let (clear, _) = token::create(pool, ADMIN_ID, "test", &scopes, &[], None)
            .await
            .expect("create token");
        clear
    }

    #[tokio::test]
    async fn pat_without_webhooks_write_cannot_delete() {
        // A real, existing webhook must not be deletable by a token that lacks
        // webhooks:write — the scope check fires before the lookup, so it 403s
        // (not 404).
        let (pool, app) = new_test_app_with_pool().await;
        let hook = webhook::create(&pool, "https://x.example.com", None, &[])
            .await
            .expect("seed webhook");
        let token = pat(&pool, &["deployments:read"]).await;
        let server = TestServer::new(app).unwrap();

        let res = server
            .delete(&format!("/webhooks/{}", hook.id))
            .add_header("Authorization", format!("Bearer {}", token))
            .await;

        assert_eq!(res.status_code(), StatusCode::FORBIDDEN);

        // And it's still there.
        assert!(
            webhook::find(&pool, &hook.id)
                .await
                .unwrap()
                .unwrap()
                .revoked_at
                .is_none()
        );
    }

    #[tokio::test]
    async fn pat_with_webhooks_write_can_delete() {
        let (pool, app) = new_test_app_with_pool().await;
        let hook = webhook::create(&pool, "https://x.example.com", None, &[])
            .await
            .expect("seed webhook");
        let token = pat(&pool, &["webhooks:write"]).await;
        let server = TestServer::new(app).unwrap();

        let res = server
            .delete(&format!("/webhooks/{}", hook.id))
            .add_header("Authorization", format!("Bearer {}", token))
            .await;

        assert_eq!(res.status_code(), StatusCode::NO_CONTENT);
    }
}
