use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::{Json, http::StatusCode};
use serde_json::json;

use crate::api::dto::user::UserOutput;
use crate::api::server::Db;
use crate::models::users as users_model;

// Scope (`users:read`) is enforced centrally by the auth middleware.
pub(crate) async fn list(State(pool): State<Db>) -> Response {
    let mut users: Vec<UserOutput> = Vec::new();

    let list_users = match users_model::find_all(&pool).await {
        Ok(list) => list,
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "errors": ["Internal server error"] })),
            )
                .into_response();
        }
    };

    for user in list_users.into_iter() {
        let output = UserOutput {
            id: user.id,
            username: user.username,
            created_at: user.created_at,
            updated_at: user.updated_at,
            status: user.status,
            login_at: user.login_at,
        };

        users.push(output);
    }

    Json(users).into_response()
}

#[cfg(test)]
mod tests {
    use crate::api::server::tests::{login, new_test_app};
    use axum_test::{TestResponse, TestServer};
    use http::StatusCode;

    #[tokio::test]
    async fn create() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .get("/users")
            .add_header("Authorization", format!("Bearer {}", token))
            .await;

        assert_eq!(response.status_code(), StatusCode::OK);
        let users = response.json::<Vec<serde_json::Value>>();
        // Check that we have at least 2 users (fixtures provide admin + john.doe, tests may add more)
        assert!(users.len() >= 2);
    }
}
