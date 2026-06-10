use crate::api::server::Db;
use crate::api::validation::problem_response;
use crate::models::token as token_model;
use crate::models::users as users_model;
use axum::extract::State;
use axum::response::Response;
use axum::{Json, http::StatusCode, response::IntoResponse};
use serde::{Deserialize, Serialize};
use serde_json::json;

pub(crate) async fn login(State(pool): State<Db>, Json(input): Json<LoginInput>) -> Response {
    debug!("Login attempt");

    let option = users_model::find_by_username(&pool, &input.username).await;

    match option {
        Ok(Some(user)) => {
            let matches = match argon2::verify_encoded(&user.password, input.password.as_bytes()) {
                Ok(m) => m,
                Err(_) => {
                    return problem_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Internal Server Error",
                        "credential verification failed",
                    );
                }
            };

            if !matches {
                return problem_response(
                    StatusCode::UNAUTHORIZED,
                    "Unauthorized",
                    "invalid credentials",
                );
            }

            // Stamp the login first, then mint the session. If the stamp fails
            // we bail before creating any token, so a failed login never leaves
            // an orphan session row (an admin-scoped credential nobody holds and
            // nothing can revoke) behind in the table.
            if users_model::login(&pool, &user).await.is_err() {
                return problem_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Internal Server Error",
                    "failed to record login",
                );
            }

            // Mint a fresh session: a row in the `token` table, `kind = session`,
            // scoped `admin` (full access, matching the old session semantics),
            // all namespaces, no expiry (revoked on logout). Every login gets its
            // own token, so two logins no longer share a secret. The clear value
            // is returned once and only its SHA-256 hash is persisted.
            let token = match token_model::create(
                &pool,
                &user.id,
                "session",
                token_model::TokenKind::Session,
                &["admin".to_string()],
                &[],
                None,
            )
            .await
            {
                Ok((clear, _)) => clear,
                Err(_) => {
                    return problem_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Internal Server Error",
                        "failed to create session",
                    );
                }
            };

            (StatusCode::OK, Json(json!({ "token": token }))).into_response()
        }
        Ok(None) => problem_response(
            StatusCode::UNAUTHORIZED,
            "Unauthorized",
            "invalid credentials",
        ),
        Err(_) => problem_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Internal Server Error",
            "lookup failed",
        ),
    }
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub(crate) struct LoginInput {
    username: String,
    password: String,
}

#[cfg(test)]
mod tests {
    use crate::api::server::tests::new_test_app;
    use axum::http::StatusCode;
    use axum_test::{TestResponse, TestServer};
    use serde::Deserialize;
    use serde_json::json;

    #[derive(Debug, Deserialize)]
    struct ResponseBody {
        token: String,
    }

    async fn login_token(server: &TestServer) -> String {
        let response: TestResponse = server
            .post("/login")
            .json(&json!({ "username": "admin", "password": "changeme" }))
            .await;
        response.json::<ResponseBody>().token
    }

    #[tokio::test]
    async fn login_success() {
        let server = TestServer::new(new_test_app().await).unwrap();
        let token = login_token(&server).await;
        assert!(!token.is_empty(), "Token key is missing in the response");
        // The session is now a row in the `token` table, so it carries the same
        // `ring_pat_` prefix as a PAT — there is one token format and one auth
        // path.
        assert!(
            token.starts_with(crate::models::token::TOKEN_PREFIX),
            "session token should be a ring_pat_ token, got: {}",
            token
        );
    }

    #[tokio::test]
    async fn session_token_authenticates_a_protected_route() {
        let server = TestServer::new(new_test_app().await).unwrap();
        let token = login_token(&server).await;

        // A session is scoped `admin`, so it must reach every protected route —
        // here a plain read.
        let ok = server
            .get("/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .await;
        assert_eq!(ok.status_code(), StatusCode::OK);
    }

    #[tokio::test]
    async fn each_login_mints_a_distinct_session() {
        let server = TestServer::new(new_test_app().await).unwrap();
        let first = login_token(&server).await;
        let second = login_token(&server).await;

        // The old bug: a 2nd login returned the *same* secret. Now every login
        // mints its own row, so the two differ and both authenticate.
        assert_ne!(first, second, "two logins must not share a token");
        for token in [&first, &second] {
            let res = server
                .get("/deployments")
                .add_header("Authorization", format!("Bearer {}", token))
                .await;
            assert_eq!(res.status_code(), StatusCode::OK);
        }
    }

    #[tokio::test]
    async fn session_is_hidden_from_the_token_list() {
        let server = TestServer::new(new_test_app().await).unwrap();
        let token = login_token(&server).await;

        // The session lives in the `token` table, but it is not a PAT the user
        // manages: `GET /tokens` must not surface it. A fresh admin has no PATs,
        // so the list is empty even though a session row exists.
        let res = server
            .get("/tokens")
            .add_header("Authorization", format!("Bearer {}", token))
            .await;
        assert_eq!(res.status_code(), StatusCode::OK);
        let list = res.json::<serde_json::Value>();
        assert_eq!(
            list.as_array().map(|a| a.len()),
            Some(0),
            "session token must not appear in /tokens, got: {}",
            list
        );
    }

    #[tokio::test]
    async fn pat_named_session_stays_visible_and_manageable() {
        // Sessions are distinguished by `kind`, not by the name "session", so a
        // user is free to name a PAT "session" — it must NOT be hidden from the
        // list or treated as a session. This is the regression the magic-name
        // model had: such a PAT used to vanish from `ring token list`.
        let server = TestServer::new(new_test_app().await).unwrap();
        let session = login_token(&server).await;

        let mint = server
            .post("/tokens")
            .add_header("Authorization", format!("Bearer {}", session))
            .json(&json!({ "name": "session", "scopes": ["deployments:read"], "namespaces": [] }))
            .await;
        assert_eq!(mint.status_code(), StatusCode::CREATED);

        // The PAT named "session" appears in the list (the session row does not).
        let list = server
            .get("/tokens")
            .add_header("Authorization", format!("Bearer {}", session))
            .await
            .json::<serde_json::Value>();
        let arr = list.as_array().expect("list");
        assert_eq!(
            arr.len(),
            1,
            "the PAT named 'session' must be listed: {}",
            list
        );
        assert_eq!(arr[0]["name"], "session");
    }

    #[tokio::test]
    async fn session_is_not_addressable_by_id_through_the_token_api() {
        // A session is hidden from the list AND off-limits by id: get/revoke/
        // rotate on /tokens/{id} must 404 for a session row, so it is managed
        // only via /login and /logout. We mint a session directly on the pool
        // (so we hold its id) and prove every id-based token route rejects it.
        use crate::models::token as token_model;
        let (pool, router) = crate::api::server::tests::new_test_app_with_pool().await;
        let server = TestServer::new(router).unwrap();
        let admin = login_token(&server).await;
        let auth = || ("Authorization", format!("Bearer {}", admin));

        // The session must be owned by the logged-in admin, otherwise find_owned
        // would 404 on ownership and mask the session-boundary check. Resolve the
        // caller's own id via /users/me, then mint a session row for it.
        let (h, v) = auth();
        let me = server.get("/users/me").add_header(h, v).await;
        let admin_id = me.json::<serde_json::Value>()["id"]
            .as_str()
            .expect("own user id")
            .to_string();
        let (_clear, session) = token_model::create(
            &pool,
            &admin_id,
            "session",
            token_model::TokenKind::Session,
            &["admin".to_string()],
            &[],
            None,
        )
        .await
        .unwrap();

        let (h, v) = auth();
        let get = server
            .get(&format!("/tokens/{}", session.id))
            .add_header(h, v)
            .await;
        assert_eq!(get.status_code(), StatusCode::NOT_FOUND);

        let (h, v) = auth();
        let del = server
            .delete(&format!("/tokens/{}", session.id))
            .add_header(h, v)
            .await;
        assert_eq!(del.status_code(), StatusCode::NOT_FOUND);

        let (h, v) = auth();
        let rot = server
            .post(&format!("/tokens/{}/rotate", session.id))
            .add_header(h, v)
            .await;
        assert_eq!(rot.status_code(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn login_fail_returns_problem_json_unauthorized() {
        let server = TestServer::new(new_test_app().await).unwrap();

        let response: TestResponse = server
            .post("/login")
            .json(&json!({
                "username": "coucou",
                "password": "changeme",
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::UNAUTHORIZED);
        let ct = response
            .header("content-type")
            .to_str()
            .unwrap()
            .to_string();
        assert!(ct.starts_with("application/problem+json"), "got: {}", ct);
        let body = response.json::<serde_json::Value>();
        assert_eq!(body["status"], 401);
        assert_eq!(body["title"], "Unauthorized");
        assert_eq!(body["detail"], "invalid credentials");
    }
}
