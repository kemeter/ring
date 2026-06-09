use crate::api::server::Db;
use crate::models::token as token_model;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};

/// `POST /logout` — revoke the session (or PAT) presented as the Bearer
/// credential. The route is behind the auth middleware, so the caller is
/// already authenticated; we resolve their own token row from the presented
/// secret and revoke that exact row. A caller can therefore only ever revoke
/// the credential they are currently holding — no id, no scope gate beyond
/// being authenticated.
///
/// Idempotent and non-revealing: an unknown or already-revoked token still
/// returns 204, so logout never leaks whether a token exists or its state.
pub(crate) async fn logout(State(pool): State<Db>, headers: HeaderMap) -> Response {
    let Some(clear) = bearer_from_headers(&headers) else {
        // Behind auth middleware this shouldn't happen, but fail safe: nothing
        // to revoke, nothing to report.
        return StatusCode::NO_CONTENT.into_response();
    };

    let hash = token_model::hash_token(&clear);
    match token_model::find_by_token_hash(&pool, &hash).await {
        Ok(Some(token)) => {
            // Best-effort: a failed revoke is logged but still returns 204 — the
            // client is logging out regardless, and we don't surface token state.
            if let Err(e) = token_model::revoke(&pool, &token.id).await {
                error!("Failed to revoke token on logout: {}", e);
            }
        }
        // Unknown token: nothing to revoke. Still 204 (non-revealing).
        Ok(None) => {}
        // A DB read error must not masquerade as "no token found": the row may
        // still be live and replayable. We log it (so a swallowed revoke is
        // diagnosable) but keep the 204 contract — the client logs out locally
        // regardless and we never leak token state.
        Err(e) => error!("Failed to look up token on logout: {}", e),
    }

    StatusCode::NO_CONTENT.into_response()
}

/// Extract the raw Bearer credential from the `Authorization` header.
fn bearer_from_headers(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    value
        .strip_prefix("Bearer ")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use crate::api::server::tests::new_test_app;
    use axum::http::StatusCode;
    use axum_test::TestServer;
    use serde_json::json;

    async fn login(server: &TestServer) -> String {
        let res = server
            .post("/login")
            .json(&json!({ "username": "admin", "password": "changeme" }))
            .await;
        res.json::<serde_json::Value>()["token"]
            .as_str()
            .unwrap()
            .to_string()
    }

    fn auth(token: &str) -> (&'static str, String) {
        ("Authorization", format!("Bearer {}", token))
    }

    #[tokio::test]
    async fn logout_revokes_the_presented_session() {
        let server = TestServer::new(new_test_app().await).unwrap();
        let token = login(&server).await;
        let (h, v) = auth(&token);

        // Token works before logout.
        let before = server.get("/deployments").add_header(h, v.clone()).await;
        assert_eq!(before.status_code(), StatusCode::OK);

        let out = server.post("/logout").add_header(h, v.clone()).await;
        assert_eq!(out.status_code(), StatusCode::NO_CONTENT);

        // Same token is now rejected — the session row was revoked.
        let after = server.get("/deployments").add_header(h, v).await;
        assert_eq!(after.status_code(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn second_logout_with_revoked_token_is_rejected_by_middleware() {
        // After the first logout the token is dead, so a replay can't even reach
        // the handler — the auth middleware rejects it with 401. (The handler's
        // own idempotency — unknown token → 204 — is unreachable here precisely
        // because a revoked token never gets past auth, which is the point.)
        let server = TestServer::new(new_test_app().await).unwrap();
        let token = login(&server).await;
        let (h, v) = auth(&token);

        let first = server.post("/logout").add_header(h, v.clone()).await;
        assert_eq!(first.status_code(), StatusCode::NO_CONTENT);
        let second = server.post("/logout").add_header(h, v).await;
        assert_eq!(second.status_code(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn fresh_login_after_logout_works() {
        let server = TestServer::new(new_test_app().await).unwrap();
        let first = login(&server).await;
        let (h, v) = auth(&first);
        server.post("/logout").add_header(h, v).await;

        // A new login mints a new, valid session.
        let second = login(&server).await;
        let (h2, v2) = auth(&second);
        let res = server.get("/deployments").add_header(h2, v2).await;
        assert_eq!(res.status_code(), StatusCode::OK);
    }
}
