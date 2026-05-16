use crate::api::action::user::validation::{
    PASSWORD_MAX, PASSWORD_MIN, USERNAME_MAX, USERNAME_MIN, USERNAME_PATTERN,
};
use crate::api::dto::user::UserOutput;
use crate::api::server::Db;
use crate::api::validation::ViolationList;
use crate::models::users as users_model;
use crate::models::users::User;
use axum::response::{IntoResponse, Response};
use axum::{Json, extract::State, http::StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::json;
use validator::Validate;

pub(crate) async fn create(
    State(pool): State<Db>,
    _user: User,
    Json(input): Json<UserInput>,
) -> Result<(StatusCode, Json<UserOutput>), Response> {
    if let Err(errs) = input.validate() {
        let violations: ViolationList = errs.into();
        return Err(violations.into_response());
    }

    let password_hash = users_model::hash_password(&input.password).map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "errors": ["Password hashing failed"] })),
        )
            .into_response()
    })?;

    users_model::create(&pool, &input.username, &password_hash)
        .await
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "errors": ["User creation failed"] })),
            )
                .into_response()
        })?;

    let user = users_model::find_by_username(&pool, &input.username)
        .await
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "errors": ["Failed to retrieve created user"] })),
            )
                .into_response()
        })?
        .ok_or_else(|| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "errors": ["Created user not found"] })),
            )
                .into_response()
        })?;

    let output = UserOutput {
        id: user.id,
        username: user.username,
        created_at: user.created_at,
        updated_at: user.updated_at,
        status: user.status,
        login_at: user.login_at,
    };

    Ok((StatusCode::CREATED, Json(output)))
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
    username: String,
    #[validate(length(
        min = "PASSWORD_MIN",
        max = "PASSWORD_MAX",
        code = "user.password.length",
        message = "must be 8 to 128 characters"
    ))]
    password: String,
}

#[cfg(test)]
mod tests {
    use crate::api::server::tests::{login, new_test_app};
    use axum_test::{TestResponse, TestServer};
    use http::StatusCode;
    use serde_json::json;

    #[tokio::test]
    async fn create() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post("/users")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "username": "ring",
                "password": "ringring"
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn create_with_short_username() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post("/users")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "username": "a",
                "password": "validpassword"
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::UNPROCESSABLE_ENTITY);
        let body = response.json::<serde_json::Value>();
        assert_eq!(body["status"], 422);
        assert_eq!(body["title"], "Validation failed");
        let v = &body["violations"];
        assert_eq!(v.as_array().unwrap().len(), 1);
        assert_eq!(v[0]["property_path"], "username");
        assert_eq!(v[0]["code"], "user.username.length");
    }

    #[tokio::test]
    async fn create_with_short_password() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post("/users")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "username": "validuser",
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
    async fn create_accumulates_all_violations() {
        // Both username and password invalid → the response must list both
        // violations in one shot, not stop at the first.
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post("/users")
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
        // username "@" trips both length and format; password "x" trips length.
        // That's three violations in total for a single request.
        assert!(codes.contains(&"user.username.length".to_string()));
        assert!(codes.contains(&"user.username.format".to_string()));
        assert!(codes.contains(&"user.password.length".to_string()));
    }

    #[tokio::test]
    async fn create_with_invalid_username_chars() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post("/users")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "username": "john doe",
                "password": "validpassword"
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::UNPROCESSABLE_ENTITY);
        let body = response.json::<serde_json::Value>();
        let v = &body["violations"];
        assert_eq!(v[0]["code"], "user.username.format");
    }

    #[tokio::test]
    async fn create_with_long_username() {
        // 51 chars → exceeds max of 50. Trips length only, not format.
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post("/users")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "username": "a".repeat(51),
                "password": "validpassword"
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
        assert!(!codes.contains(&"user.username.format".to_string()));
    }

    #[tokio::test]
    async fn create_unauthenticated_does_not_validate() {
        // Auth middleware must short-circuit before validation runs —
        // protects against unauthenticated callers fingerprinting our
        // field rules.
        let app = new_test_app().await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post("/users")
            .json(&json!({
                "username": "@",
                "password": "x"
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn create_response_uses_problem_json_content_type() {
        // The 422 must carry `application/problem+json` so clients that
        // branch on Content-Type can parse the body as an RFC 7807
        // payload without sniffing.
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post("/users")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({ "username": "a", "password": "short" }))
            .await;

        assert_eq!(response.status_code(), StatusCode::UNPROCESSABLE_ENTITY);
        let content_type = response
            .header("content-type")
            .to_str()
            .unwrap()
            .to_string();
        assert!(
            content_type.starts_with("application/problem+json"),
            "got content-type: {}",
            content_type
        );
    }
}
