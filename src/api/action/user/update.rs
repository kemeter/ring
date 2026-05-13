use crate::api::action::user::validation::{
    PASSWORD_MAX, PASSWORD_MIN, USERNAME_MAX, USERNAME_MIN, USERNAME_PATTERN,
};
use crate::api::server::Db;
use crate::api::validation::ViolationList;
use crate::config::config::Config;
use crate::models::users as users_model;
use crate::models::users::User;
use axum::extract::State;
use axum::{Json, extract::Path, response::IntoResponse};
use http::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::json;
use validator::Validate;

pub(crate) async fn update(
    State(pool): State<Db>,
    State(configuration): State<Config>,
    Path(id): Path<String>,
    _user: User,
    Json(input): Json<UserInput>,
) -> Result<impl IntoResponse, impl IntoResponse> {
    // `Validate` skips fields that are `None`, so an empty body falls
    // through cleanly. Whatever the user passes gets the same rules as
    // create — the regex / length attributes are declared once and shared
    // via constants in `validation.rs`.
    if let Err(errs) = input.validate() {
        let violations: ViolationList = errs.into();
        return Err(violations.into_response());
    }

    let mut user = match users_model::find(&pool, &id).await.ok().flatten() {
        Some(user) => user,
        None => return Err((StatusCode::NOT_FOUND, "User not found").into_response()),
    };

    if let Some(username) = input.username {
        user.username = username;
    }

    if let Some(password) = input.password {
        let password_hash = users_model::hash_password(&password, &configuration.user.salt)
            .map_err(|_| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "errors": ["Password hashing failed"] })),
                )
                    .into_response()
            })?;

        user.password = password_hash;
    }

    if users_model::update(&pool, &user).await.is_err() {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "errors": ["Failed to update user"] })),
        )
            .into_response());
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
}

#[cfg(test)]
mod tests {
    use crate::api::server::tests::{login, new_test_app};
    use axum_test::{TestResponse, TestServer};
    use http::StatusCode;
    use serde_json::json;

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
