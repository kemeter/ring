use crate::api::auth::Auth;
use crate::api::server::Db;
use crate::api::validation::{Violation, ViolationList, problem_response};
use crate::events::validate_event_filter;
use crate::models::audit_log;
use crate::models::webhook;
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize, Debug, Clone)]
pub(crate) struct WebhookInput {
    url: String,
    /// Event kinds to subscribe to. Omitted or empty = all kinds.
    #[serde(default)]
    events: Vec<String>,
    /// Optional HMAC secret. When omitted, Ring generates one and returns it
    /// once in the response.
    #[serde(default)]
    secret: Option<String>,
}

/// Returned only by create: the secret is shown once here and never again.
#[derive(Serialize)]
struct WebhookCreated {
    id: String,
    url: String,
    events: Vec<String>,
    secret: Option<String>,
    created_at: String,
    message: String,
}

pub(crate) async fn create(
    State(pool): State<Db>,
    auth: Auth,
    Json(input): Json<WebhookInput>,
) -> Response {
    // Scope (`webhooks:write`) is enforced centrally by the auth middleware
    // via scope_for_route; the handler only validates and persists.
    let mut violations = ViolationList::new();

    // URL must be http(s) AND must not target an internal address: the worker
    // POSTs to it server-side, so an unrestricted URL is an SSRF primitive
    // (loopback, RFC-1918, cloud metadata). Reachability is still not checked —
    // delivery failures are handled by the worker's retry/dead-letter.
    if let Some(reason) = webhook::url_safety_violation(&input.url) {
        violations.push(Violation::new("url", reason, "webhook.url.format"));
    }

    // Each filter entry must be a known kind, a known `family.*`, or `*`. A
    // malformed entry (typo, missing dot) is rejected loudly rather than
    // silently never matching.
    for entry in &input.events {
        if let Err(reason) = validate_event_filter(entry) {
            violations.push(Violation::new("events", reason, "webhook.events.unknown"));
        }
    }

    // A caller-supplied secret is intentionally unconstrained: it's the
    // subscriber's own shared secret, not a Ring credential, so its strength is
    // the caller's call. When omitted, Ring generates a strong one.

    if !violations.is_empty() {
        return violations.into_response();
    }

    // Use the caller's secret if given, otherwise generate one.
    let secret = input
        .secret
        .clone()
        .unwrap_or_else(webhook::generate_secret);

    match webhook::create(&pool, &input.url, Some(&secret), &input.events).await {
        Ok(created) => {
            let _ = audit_log::record(
                &pool,
                Some(&auth.user.id),
                "create",
                "webhook",
                &created.url,
                None,
            )
            .await;
            let output = WebhookCreated {
                id: created.id,
                url: created.url,
                events: created.events,
                secret: Some(secret),
                created_at: created.created_at,
                message: "Copy the secret now — it will not be shown again.".to_string(),
            };
            (StatusCode::CREATED, Json(output)).into_response()
        }
        Err(e) => {
            log::error!("Failed to create webhook: {}", e);
            problem_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Internal Server Error",
                "failed to create webhook",
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::api::server::tests::{login, new_test_app_with_pool};
    use crate::models::token;
    use crate::models::webhook;
    use axum_test::TestServer;
    use http::StatusCode;
    use serde_json::json;

    const ADMIN_ID: &str = "1c5a5fe9-84e0-4a18-821e-8058232c2c23";

    /// Mint a PAT with the given scopes and return its clear `ring_pat_…` value.
    async fn pat(pool: &sqlx::SqlitePool, scopes: &[&str]) -> String {
        let scopes: Vec<String> = scopes.iter().map(|s| s.to_string()).collect();
        let (clear, _) = token::create(pool, ADMIN_ID, "test", &scopes, &[], None)
            .await
            .expect("create token");
        clear
    }

    fn body() -> serde_json::Value {
        json!({ "url": "https://hooks.example.com/ring", "events": ["deployment.status_changed"] })
    }

    #[tokio::test]
    async fn session_bearer_can_create_webhook() {
        // A logged-in human (unscoped session) has full access.
        let (pool, app) = new_test_app_with_pool().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let _ = &pool;
        let server = TestServer::new(app).unwrap();

        let res = server
            .post("/webhooks")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&body())
            .await;

        assert_eq!(res.status_code(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn pat_with_webhooks_write_can_create() {
        let (pool, app) = new_test_app_with_pool().await;
        let token = pat(&pool, &["webhooks:write"]).await;
        let server = TestServer::new(app).unwrap();

        let res = server
            .post("/webhooks")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&body())
            .await;

        assert_eq!(res.status_code(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn pat_without_webhooks_write_is_forbidden() {
        // The whole point: a token that lacks webhooks:write must NOT be able to
        // register a webhook, even though it is otherwise a valid credential.
        let (pool, app) = new_test_app_with_pool().await;
        let token = pat(&pool, &["deployments:read", "secrets:read"]).await;
        let server = TestServer::new(app).unwrap();

        let res = server
            .post("/webhooks")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&body())
            .await;

        assert_eq!(res.status_code(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn read_scope_alone_cannot_create() {
        // webhooks:read lets you list, not create.
        let (pool, app) = new_test_app_with_pool().await;
        let token = pat(&pool, &["webhooks:read"]).await;
        let server = TestServer::new(app).unwrap();

        let res = server
            .post("/webhooks")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&body())
            .await;

        assert_eq!(res.status_code(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn no_token_is_unauthorized() {
        let (_pool, app) = new_test_app_with_pool().await;
        let server = TestServer::new(app).unwrap();

        let res = server.post("/webhooks").json(&body()).await;

        assert_eq!(res.status_code(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn admin_scope_can_create() {
        // admin is a wildcard over every scope, webhooks:write included.
        let (pool, app) = new_test_app_with_pool().await;
        let token = pat(&pool, &["admin"]).await;
        let server = TestServer::new(app).unwrap();

        let res = server
            .post("/webhooks")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&body())
            .await;

        assert_eq!(res.status_code(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn create_rejects_unknown_event_kind() {
        // Subscribing to a kind Ring never emits is a 422, and nothing is
        // persisted — guards the KNOWN_EVENT_KINDS validation.
        let (pool, app) = new_test_app_with_pool().await;
        let token = pat(&pool, &["webhooks:write"]).await;
        let server = TestServer::new(app).unwrap();

        let res = server
            .post("/webhooks")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({ "url": "https://hooks.example.com/ring", "events": ["bogus.kind"] }))
            .await;

        assert_eq!(res.status_code(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            webhook::find_all(&pool).await.unwrap().is_empty(),
            "no webhook persisted on validation failure"
        );
    }

    #[tokio::test]
    async fn create_accepts_wildcard_filters() {
        // `*` and `<family>.*` are valid subscription filters.
        let (pool, app) = new_test_app_with_pool().await;
        let token = pat(&pool, &["webhooks:write"]).await;
        let server = TestServer::new(app).unwrap();

        for filter in ["*", "deployment.*"] {
            let res = server
                .post("/webhooks")
                .add_header("Authorization", format!("Bearer {}", token))
                .json(&json!({ "url": "https://hooks.example.com/ring", "events": [filter] }))
                .await;
            assert_eq!(
                res.status_code(),
                StatusCode::CREATED,
                "filter {filter:?} should be accepted"
            );
        }
    }

    #[tokio::test]
    async fn create_rejects_malformed_wildcard_with_helpful_message() {
        // `deployment*` (missing dot) is the classic typo — it must 422 with a
        // message that tells the caller the right form, not silently never match.
        let (pool, app) = new_test_app_with_pool().await;
        let token = pat(&pool, &["webhooks:write"]).await;
        let server = TestServer::new(app).unwrap();

        let res = server
            .post("/webhooks")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({ "url": "https://hooks.example.com/ring", "events": ["deployment*"] }))
            .await;

        assert_eq!(res.status_code(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(res.text().contains("invalid wildcard"), "{}", res.text());
        assert!(webhook::find_all(&pool).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn create_accepts_any_caller_secret() {
        // A caller-supplied secret is unconstrained — its strength is the
        // caller's call, so even a short one is accepted and persisted.
        let (pool, app) = new_test_app_with_pool().await;
        let token = pat(&pool, &["webhooks:write"]).await;
        let server = TestServer::new(app).unwrap();

        let res = server
            .post("/webhooks")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({ "url": "https://hooks.example.com/ring", "secret": "short" }))
            .await;

        assert_eq!(res.status_code(), StatusCode::CREATED);
        assert_eq!(webhook::find_all(&pool).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn create_rejects_internal_url() {
        // SSRF guard: a URL pointing at an internal address is a 422.
        let (pool, app) = new_test_app_with_pool().await;
        let token = pat(&pool, &["webhooks:write"]).await;
        let server = TestServer::new(app).unwrap();

        let res = server
            .post("/webhooks")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({ "url": "http://169.254.169.254/latest/meta-data", "events": [] }))
            .await;

        assert_eq!(res.status_code(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(webhook::find_all(&pool).await.unwrap().is_empty());
    }
}
