use crate::api::action::user::validation::{
    PASSWORD_MAX, PASSWORD_MIN, USERNAME_MAX, USERNAME_MIN, USERNAME_PATTERN,
};
use crate::api::auth::Auth;
use crate::api::server::Db;
use crate::api::validation::ViolationList;
use crate::models::token as token_model;
use crate::models::users as users_model;
use crate::models::users::Role;
use axum::extract::State;
use axum::{Json, extract::Path, response::IntoResponse};
use http::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::json;
use validator::Validate;

pub(crate) async fn update(
    State(pool): State<Db>,
    Path(id): Path<String>,
    auth: Auth,
    Json(input): Json<UserInput>,
) -> Result<impl IntoResponse, impl IntoResponse> {
    // Authorization: a user may only update its own account; an admin may
    // update any. Without this an authenticated user could overwrite any
    // other user's password and take the account over (IDOR).
    if auth.user.id != id && !auth.user.is_admin() {
        return Err((StatusCode::FORBIDDEN, "Forbidden").into_response());
    }

    // `Validate` skips fields that are `None`, so an empty body falls
    // through cleanly. Whatever the user passes gets the same rules as
    // create — the regex / length attributes are declared once and shared
    // via constants in `validation.rs`.
    if let Err(errs) = input.validate() {
        let violations: ViolationList = errs.into();
        return Err(violations.into_response());
    }

    // Scope (`users:write`) is enforced centrally by the auth middleware; the
    // self/admin authorization above is an additional, finer-grained check.
    let mut user = match users_model::find(&pool, &id).await.ok().flatten() {
        Some(user) => user,
        None => return Err((StatusCode::NOT_FOUND, "User not found").into_response()),
    };

    if let Some(username) = input.username {
        user.username = username;
    }

    if let Some(password) = input.password {
        let password_hash = users_model::hash_password(&password).map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "errors": ["Password hashing failed"] })),
            )
                .into_response()
        })?;

        user.password = password_hash;
    }

    // Changing a role is an administrative act, never self-service: a viewer
    // must not be able to promote itself by PUTing its own account.
    let role_changed = match input.role {
        Some(requested) if requested != user.role => {
            if !auth.user.is_admin() {
                return Err((StatusCode::FORBIDDEN, "Forbidden").into_response());
            }

            let role = Role::parse(&requested).map_err(|_| {
                (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "errors": ["Unknown role"] })),
                )
                    .into_response()
            })?;

            // Never demote the last admin: doing so leaves nobody able to
            // administer the instance, with no way back in through the API.
            if user.is_admin()
                && role != Role::Admin
                && users_model::count_admins(&pool).await.unwrap_or(0) <= 1
            {
                return Err((
                    StatusCode::CONFLICT,
                    Json(json!({ "errors": ["Cannot demote the last admin"] })),
                )
                    .into_response());
            }

            user.role = role.as_str().to_string();
            true
        }
        _ => false,
    };

    if users_model::update(&pool, &user).await.is_err() {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "errors": ["Failed to update user"] })),
        )
            .into_response());
    }

    // A role change only takes effect once the account's existing credentials
    // are revoked: scopes are frozen into each token row at mint time, so a
    // demoted admin would otherwise keep full access through the session and
    // PATs they already hold. Revoking here is what makes RBAC enforceable.
    if role_changed {
        match token_model::revoke_all_for_user(&pool, &user.id).await {
            Ok(revoked) => {
                info!(
                    user_id = %user.id,
                    role = %user.role,
                    revoked,
                    "role changed: revoked the account's credentials"
                );
            }
            Err(err) => {
                // Fail loudly: reporting success here would leave the operator
                // believing a demotion took effect while the old privileges
                // remain usable.
                error!(user_id = %user.id, error = %err, "failed to revoke credentials after role change");
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(
                        json!({ "errors": ["Role updated but credentials could not be revoked"] }),
                    ),
                )
                    .into_response());
            }
        }
    }

    Ok(StatusCode::OK.into_response())
}

#[derive(Deserialize, Serialize, Debug, Clone, Validate)]
pub(crate) struct UserInput {
    #[validate(
        length(
            min = "USERNAME_MIN",
            max = "USERNAME_MAX",
            code = "user.username.length",
            message = "must be 2 to 50 characters"
        ),
        regex(
            path = *USERNAME_PATTERN,
            code = "user.username.format",
            message = "must start with a letter or digit and contain only letters, digits, '.', '-', '_'"
        )
    )]
    username: Option<String>,
    #[validate(length(
        min = "PASSWORD_MIN",
        max = "PASSWORD_MAX",
        code = "user.password.length",
        message = "must be 8 to 128 characters"
    ))]
    password: Option<String>,
    /// Target role (`admin`, `operator`, `viewer`). Admin-only, and validated
    /// against the known roles in the handler rather than by a length rule.
    role: Option<String>,
}

#[cfg(test)]
mod tests {
    use crate::api::server::tests::{login, new_test_app, new_test_app_with_pool};
    use crate::models::users as users_model;
    use axum_test::{TestResponse, TestServer};
    use http::StatusCode;
    use serde_json::json;

    const JOHN_ID: &str = "6c6d481b-debf-5gb5-937f-2ffa5e9f8e58";
    const ADMIN_ID: &str = "1c5a5fe9-84e0-4a18-821e-8058232c2c23";

    /// Count an account's live credentials, sessions included.
    ///
    /// `token::find_all_for_user` deliberately hides sessions (it backs
    /// `ring token list`, which only shows PATs), so it cannot answer "is this
    /// user still logged in?" -- exactly what a revocation-on-demotion test
    /// needs to assert.
    async fn live_credentials(pool: &sqlx::SqlitePool, user_id: &str) -> i64 {
        let (count,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM token WHERE user_id = ? AND revoked_at IS NULL")
                .bind(user_id)
                .fetch_one(pool)
                .await
                .unwrap();

        count
    }

    #[tokio::test]
    async fn admin_can_change_a_role_and_it_revokes_the_account_credentials() {
        // The point of the commit: scopes are frozen into each token row at
        // mint time, so a role change is only enforceable if the account's
        // existing credentials are revoked.
        let (pool, app) = new_test_app_with_pool().await;
        let admin_token = login(app.clone(), "admin", "changeme").await;
        // john.doe logs in, so there is a live session to invalidate.
        let john_token = login(app.clone(), "john.doe", "changeme").await;
        let server = TestServer::new(app).unwrap();

        assert!(!john_token.is_empty());
        assert!(
            live_credentials(&pool, JOHN_ID).await > 0,
            "john.doe should hold a live session before the demotion"
        );

        let response: TestResponse = server
            .put(&format!("/users/{JOHN_ID}"))
            .add_header("Authorization", format!("Bearer {}", admin_token))
            .json(&json!({ "role": "operator" }))
            .await;
        assert_eq!(response.status_code(), StatusCode::OK);

        let user = users_model::find(&pool, JOHN_ID).await.unwrap().unwrap();
        assert_eq!(user.role, "operator", "the new role must be persisted");

        assert_eq!(
            live_credentials(&pool, JOHN_ID).await,
            0,
            "every credential of the account must be revoked on a role change"
        );
    }

    #[tokio::test]
    async fn unchanged_role_does_not_revoke_credentials() {
        // Only an actual change revokes: re-sending the current role (or
        // updating another field) must not log the user out.
        let (pool, app) = new_test_app_with_pool().await;
        let admin_token = login(app.clone(), "admin", "changeme").await;
        login(app.clone(), "john.doe", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .put(&format!("/users/{JOHN_ID}"))
            .add_header("Authorization", format!("Bearer {}", admin_token))
            .json(&json!({ "role": "viewer", "username": "john.doe" }))
            .await;
        assert_eq!(response.status_code(), StatusCode::OK);

        assert!(
            live_credentials(&pool, JOHN_ID).await > 0,
            "an unchanged role must not revoke the account's session"
        );
    }

    #[tokio::test]
    async fn non_admin_cannot_change_its_own_role() {
        // Self-service must not become a promotion path.
        let app = new_test_app().await;
        let token = login(app.clone(), "john.doe", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .put(&format!("/users/{JOHN_ID}"))
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({ "role": "admin" }))
            .await;

        assert_eq!(response.status_code(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn unknown_role_is_rejected() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .put(&format!("/users/{JOHN_ID}"))
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({ "role": "root" }))
            .await;

        assert_eq!(response.status_code(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn the_last_admin_cannot_be_demoted() {
        // Demoting the only admin would leave the instance unadministrable
        // with no way back in through the API.
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .put(&format!("/users/{ADMIN_ID}"))
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({ "role": "viewer" }))
            .await;

        assert_eq!(response.status_code(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn update_not_found() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .put("/users/non-existent-id")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "username": "newname"
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn update_username() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .put("/users/1c5a5fe9-84e0-4a18-821e-8058232c2c23")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "username": "newadmin"
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::OK);

        let me_response = server
            .get("/users/me")
            .add_header("Authorization", format!("Bearer {}", token))
            .await;

        let user = me_response.json::<serde_json::Value>();
        assert_eq!(user["username"], "newadmin");
    }

    #[tokio::test]
    async fn update_password() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .put("/users/1c5a5fe9-84e0-4a18-821e-8058232c2c23")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "password": "newpassword"
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::OK);
    }

    #[tokio::test]
    async fn update_with_short_username_returns_violations() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .put("/users/1c5a5fe9-84e0-4a18-821e-8058232c2c23")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "username": "a"
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::UNPROCESSABLE_ENTITY);
        let body = response.json::<serde_json::Value>();
        let v = &body["violations"];
        assert_eq!(v.as_array().unwrap().len(), 1);
        assert_eq!(v[0]["property_path"], "username");
        assert_eq!(v[0]["code"], "user.username.length");
    }

    #[tokio::test]
    async fn update_with_invalid_username_chars_returns_violations() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .put("/users/1c5a5fe9-84e0-4a18-821e-8058232c2c23")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "username": "john doe"
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::UNPROCESSABLE_ENTITY);
        let body = response.json::<serde_json::Value>();
        let v = &body["violations"];
        assert_eq!(v[0]["code"], "user.username.format");
    }

    #[tokio::test]
    async fn update_with_short_password_returns_violations() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .put("/users/1c5a5fe9-84e0-4a18-821e-8058232c2c23")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "password": "short"
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::UNPROCESSABLE_ENTITY);
        let body = response.json::<serde_json::Value>();
        let v = &body["violations"];
        assert_eq!(v.as_array().unwrap().len(), 1);
        assert_eq!(v[0]["property_path"], "password");
        assert_eq!(v[0]["code"], "user.password.length");
    }

    #[tokio::test]
    async fn update_accumulates_all_violations() {
        // Both fields invalid → response must list everything in one shot.
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .put("/users/1c5a5fe9-84e0-4a18-821e-8058232c2c23")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "username": "@",
                "password": "x"
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::UNPROCESSABLE_ENTITY);
        let body = response.json::<serde_json::Value>();
        let codes: Vec<String> = body["violations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["code"].as_str().unwrap().to_string())
            .collect();
        assert!(codes.contains(&"user.username.length".to_string()));
        assert!(codes.contains(&"user.username.format".to_string()));
        assert!(codes.contains(&"user.password.length".to_string()));
    }

    #[tokio::test]
    async fn update_empty_body_is_a_noop_with_ok() {
        // PUT with neither username nor password: no validation triggers,
        // no field changes, and the existing user comes back unchanged.
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .put("/users/1c5a5fe9-84e0-4a18-821e-8058232c2c23")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({}))
            .await;

        assert_eq!(response.status_code(), StatusCode::OK);
    }

    #[tokio::test]
    async fn non_admin_cannot_update_another_user() {
        // IDOR regression: john.doe (role=user) must NOT be able to change
        // the admin account's credentials. Expect 403, before validation.
        let app = new_test_app().await;
        let token = login(app.clone(), "john.doe", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .put("/users/5b5c370a-cdbf-4fa4-826e-1eea4d8f7d47") // admin's id
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({ "password": "pwned-by-john" }))
            .await;

        assert_eq!(response.status_code(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn non_admin_can_update_own_account() {
        // Self-service must still work for a plain user.
        let app = new_test_app().await;
        let token = login(app.clone(), "john.doe", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .put("/users/6c6d481b-debf-5gb5-937f-2ffa5e9f8e58") // john.doe's id
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({ "username": "john.doe2" }))
            .await;

        assert_eq!(response.status_code(), StatusCode::OK);
    }

    #[tokio::test]
    async fn admin_can_update_another_user() {
        // Admin retains the ability to manage other accounts.
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .put("/users/6c6d481b-debf-5gb5-937f-2ffa5e9f8e58") // john.doe's id
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({ "username": "john.renamed" }))
            .await;

        assert_eq!(response.status_code(), StatusCode::OK);
    }

    #[tokio::test]
    async fn update_unauthenticated_does_not_validate() {
        // No bearer token: the auth middleware must short-circuit with
        // 401 before validation runs — we don't want validation behavior
        // to leak field names to unauthenticated callers.
        let app = new_test_app().await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .put("/users/1c5a5fe9-84e0-4a18-821e-8058232c2c23")
            .json(&json!({
                "username": "@"
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::UNAUTHORIZED);
    }
}
