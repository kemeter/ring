use crate::api::auth::Auth;
use crate::api::server::Db;
use crate::api::validation::problem_response;
use crate::models::event_queue::{self, QueuedEvent};
use crate::models::webhook;
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

/// Default page size when the caller doesn't pass `?limit=`. Picked to fit a
/// terminal screen on `ring webhook inspect` without scrolling.
const DEFAULT_LIMIT: i64 = 50;

/// Hard ceiling: a caller can't ask for an unbounded scan of the events table
/// from a single request — the model already widens the SQL window 4× to
/// account for filtering, and the table is read on the same SQLite pool the
/// worker uses.
const MAX_LIMIT: i64 = 200;

/// GET /webhooks/{id}/events
///
/// Recent events offered to this webhook, newest first. Returns 404 if the
/// webhook id is unknown, 200 with an empty list if it exists but no event has
/// matched yet (including for revoked webhooks: inspect is also an audit aid).
pub(crate) async fn events(
    State(pool): State<Db>,
    Path(id): Path<String>,
    _auth: Auth,
) -> Response {
    // 404 vs empty 200: a missing webhook is a client error (wrong id), an
    // existing webhook with no matches is a normal empty result. The model
    // collapses both to `Ok(vec![])`, so we re-check existence here.
    match webhook::find(&pool, &id).await {
        Ok(Some(_)) => {}
        Ok(None) => {
            return problem_response(StatusCode::NOT_FOUND, "Not Found", "webhook not found");
        }
        Err(e) => {
            error!("Failed to load webhook {}: {}", id, e);
            return problem_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Internal Server Error",
                "failed to load webhook",
            );
        }
    }

    match event_queue::find_for_webhook(&pool, &id, DEFAULT_LIMIT.min(MAX_LIMIT)).await {
        Ok(events) => {
            let views: Vec<QueuedEvent> = events;
            (StatusCode::OK, Json(views)).into_response()
        }
        Err(e) => {
            error!("Failed to list events for webhook {}: {}", id, e);
            problem_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Internal Server Error",
                "failed to list events",
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::api::server::tests::new_test_app_with_pool;
    use crate::models::event_queue;
    use crate::models::token;
    use crate::models::webhook;
    use axum_test::TestServer;
    use http::StatusCode;

    const ADMIN_ID: &str = "1c5a5fe9-84e0-4a18-821e-8058232c2c23";

    async fn pat(pool: &sqlx::SqlitePool, scopes: &[&str]) -> String {
        let scopes: Vec<String> = scopes.iter().map(|s| s.to_string()).collect();
        let (clear, _) = token::create(
            pool,
            ADMIN_ID,
            "test",
            token::TokenKind::Pat,
            &scopes,
            &[],
            None,
        )
        .await
        .expect("create token");
        clear
    }

    #[tokio::test]
    async fn unknown_webhook_id_returns_404() {
        // Distinct from "webhook exists but has no matching events": the latter
        // is a 200 with []. A 404 only fires when the id itself is invalid, so
        // a CLI typo is surfaced explicitly instead of silently returning [].
        let (pool, app) = new_test_app_with_pool().await;
        let token = pat(&pool, &["webhooks:read"]).await;
        let server = TestServer::new(app).unwrap();

        let res = server
            .get("/webhooks/does-not-exist/events")
            .add_header("Authorization", format!("Bearer {}", token))
            .await;

        assert_eq!(res.status_code(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn returns_only_events_matching_the_subscription_filter() {
        // Same invariant as the model test, but exercised through the HTTP
        // surface: a webhook subscribed to deployment.* sees only those, not
        // node.* siblings. Catches regressions where the endpoint forgets to
        // delegate filtering and dumps the whole queue.
        let (pool, app) = new_test_app_with_pool().await;
        let hook = webhook::create(&pool, "https://x", None, &["deployment.*".to_string()])
            .await
            .unwrap();
        event_queue::enqueue(&pool, "deployment.status_changed", "{\"k\":1}")
            .await
            .unwrap();
        event_queue::enqueue(&pool, "node.online", "{}")
            .await
            .unwrap();
        event_queue::enqueue(&pool, "deployment.scaled", "{\"k\":2}")
            .await
            .unwrap();
        let token = pat(&pool, &["webhooks:read"]).await;
        let server = TestServer::new(app).unwrap();

        let res = server
            .get(&format!("/webhooks/{}/events", hook.id))
            .add_header("Authorization", format!("Bearer {}", token))
            .await;

        assert_eq!(res.status_code(), StatusCode::OK);
        let body = res.text();
        assert!(body.contains("deployment.scaled"));
        assert!(body.contains("deployment.status_changed"));
        assert!(
            !body.contains("node.online"),
            "leaked non-matching kind: {body}"
        );
    }
}
