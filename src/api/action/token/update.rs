use crate::api::action::token::validation::validate_scopes;
use crate::api::action::token::{TokenView, find_owned};
use crate::api::auth::Auth;
use crate::api::server::Db;
use crate::api::validation::{ViolationList, problem_response};
use crate::models::audit_log;
use crate::models::token;
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

#[derive(Deserialize)]
pub(crate) struct TokenUpdateInput {
    scopes: Vec<String>,
}

/// Change what a token may do without changing the token itself.
///
/// Rotating mints a new value, which every holder then has to be handed again.
/// When a service only needs one more scope, that is a secret to redistribute
/// for no reason, and a window where the old value is revoked but the new one
/// not yet deployed. Updating keeps the value and replaces the scopes, which go
/// through the same validation as at creation.
pub(crate) async fn update(
    Path(id): Path<String>,
    State(pool): State<Db>,
    auth: Auth,
    Json(input): Json<TokenUpdateInput>,
) -> Response {
    let existing = match find_owned(&pool, &id, &auth.user.id).await {
        Ok(t) => t,
        Err(resp) => return resp,
    };

    if existing.revoked_at.is_some() {
        return problem_response(
            StatusCode::CONFLICT,
            "Conflict",
            "a revoked token cannot be updated",
        );
    }

    let mut violations = ViolationList::new();
    validate_scopes(&input.scopes, &mut violations);
    if !violations.is_empty() {
        return violations.into_response();
    }

    if let Err(e) = token::update_scopes(&pool, &existing.id, &input.scopes).await {
        error!("Failed to update token scopes: {}", e);
        return problem_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Internal Server Error",
            "failed to update token",
        );
    }

    let _ = audit_log::record(
        &pool,
        Some(&auth.user.id),
        "update",
        "token",
        &existing.name,
        None,
    )
    .await;

    match find_owned(&pool, &existing.id, &auth.user.id).await {
        Ok(t) => (StatusCode::OK, Json(TokenView::from(t))).into_response(),
        Err(resp) => resp,
    }
}

#[cfg(test)]
mod tests {
    use crate::api::server::tests::{login, new_test_app};
    use axum::http::StatusCode;
    use axum_test::TestServer;
    use serde_json::{Value, json};

    /// Mint a PAT with the admin session, and return its id and clear value.
    async fn mint(server: &TestServer, session: &str, scopes: &[&str]) -> (String, String) {
        let created: Value = server
            .post("/tokens")
            .add_header("Authorization", format!("Bearer {}", session))
            .json(&json!({ "name": "service", "scopes": scopes, "namespaces": [] }))
            .await
            .json();

        (
            created["id"].as_str().unwrap().to_string(),
            created["token"].as_str().unwrap().to_string(),
        )
    }

    /// The point of updating rather than rotating: the value a service already
    /// holds gains the new scope, with nothing to redistribute.
    #[tokio::test]
    async fn updating_the_scopes_takes_effect_on_the_same_value() {
        let app = new_test_app().await;
        let session = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app);
        let (id, pat) = mint(&server, &session, &["deployments:read"]).await;

        let before = server
            .get("/volumes")
            .add_header("Authorization", format!("Bearer {}", pat))
            .await;
        assert_eq!(before.status_code(), StatusCode::FORBIDDEN);

        let response = server
            .patch(&format!("/tokens/{}", id))
            .add_header("Authorization", format!("Bearer {}", session))
            .json(&json!({ "scopes": ["deployments:read", "volumes:read"] }))
            .await;
        assert_eq!(response.status_code(), StatusCode::OK);
        let updated: Value = response.json();
        assert_eq!(
            updated["scopes"],
            json!(["deployments:read", "volumes:read"])
        );
        assert!(
            updated.get("token").is_none(),
            "the value is never sent back"
        );

        let after = server
            .get("/volumes")
            .add_header("Authorization", format!("Bearer {}", pat))
            .await;
        assert_eq!(after.status_code(), StatusCode::OK);
    }

    #[tokio::test]
    async fn an_unknown_scope_is_refused() {
        let app = new_test_app().await;
        let session = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app);
        let (id, _) = mint(&server, &session, &["deployments:read"]).await;

        let response = server
            .patch(&format!("/tokens/{}", id))
            .add_header("Authorization", format!("Bearer {}", session))
            .json(&json!({ "scopes": ["volumes:destroy"] }))
            .await;

        assert_eq!(response.status_code(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn an_unknown_token_is_not_found() {
        let app = new_test_app().await;
        let session = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app);

        let response = server
            .patch("/tokens/00000000-0000-0000-0000-000000000000")
            .add_header("Authorization", format!("Bearer {}", session))
            .json(&json!({ "scopes": ["deployments:read"] }))
            .await;

        assert_eq!(response.status_code(), StatusCode::NOT_FOUND);
    }

    /// A revoked token stays revoked: giving it scopes would suggest otherwise.
    #[tokio::test]
    async fn a_revoked_token_cannot_be_updated() {
        let app = new_test_app().await;
        let session = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app);
        let (id, _) = mint(&server, &session, &["deployments:read"]).await;
        server
            .delete(&format!("/tokens/{}", id))
            .add_header("Authorization", format!("Bearer {}", session))
            .await;

        let response = server
            .patch(&format!("/tokens/{}", id))
            .add_header("Authorization", format!("Bearer {}", session))
            .json(&json!({ "scopes": ["admin"] }))
            .await;

        assert_eq!(response.status_code(), StatusCode::CONFLICT);
    }

    /// Like minting and rotating, widening a token is an admin's call: a lesser
    /// PAT must not be able to give itself, or another token, more power.
    #[tokio::test]
    async fn a_non_admin_token_cannot_update_scopes() {
        let app = new_test_app().await;
        let session = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app);
        let (id, pat) = mint(&server, &session, &["users:write", "deployments:write"]).await;

        let response = server
            .patch(&format!("/tokens/{}", id))
            .add_header("Authorization", format!("Bearer {}", pat))
            .json(&json!({ "scopes": ["admin"] }))
            .await;

        assert_eq!(response.status_code(), StatusCode::FORBIDDEN);
    }
}
