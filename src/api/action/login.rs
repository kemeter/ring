use axum::{
    http::StatusCode,
    response::IntoResponse,
    Json
};
use axum::extract::State;
use serde::{Serialize, Deserialize};
use serde_json::json;
use crate::api::server::Db;
use crate::models::users as users_model;
use rand::Rng;
use rand::rng;
use rand::distr::Alphanumeric;

pub(crate) async fn login(
    State(connexion): State<Db>,
    Json(input): Json<LoginInput>
) -> impl IntoResponse {
    debug!("Login attempt");

    let option = {
        let guard = connexion.lock().await;
        users_model::find_by_username(&guard, &input.username)
    };

    match option {
        Ok(Some(mut user)) => {
            let matches = match argon2::verify_encoded(&user.password, input.password.as_bytes()) {
                Ok(m) => m,
                Err(_) => return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "errors": ["Internal server error"] }))
                ),
            };

            if !matches {
                return (
                    StatusCode::UNAUTHORIZED,
                    Json(json!({ "errors": ["Invalid credentials"] }))
                );
            }

            if user.token.is_empty() {
                user.token = generate_token();
            }

            let token: String = user.token.clone();

            {
                let guard = connexion.lock().await;
                if let Err(_) = users_model::login(&guard, user) {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(json!({ "errors": ["Internal server error"] }))
                    );
                }
            }

            (
                StatusCode::OK,
                Json(json!({ "token": token }))
            )
        }
        Ok(None) => (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "errors": ["Invalid credentials"] }))
        ),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "errors": ["Internal server error"] }))
        ),
    }
}

fn generate_token() -> String {
    format!(
        "tk_{}_{}",
        chrono::Utc::now().timestamp(),
        rng()
            .sample_iter(&Alphanumeric)
            .take(24)
            .map(char::from)
            .collect::<String>()
    )
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub(crate) struct HttpResponse {
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    errors: Vec<String>,

    #[serde(default, skip_serializing_if = "String::is_empty")]
    token: String
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub(crate) struct LoginInput {
    username: String,
    password: String
}

#[cfg(test)]
mod tests {
    use crate::api::server::tests::new_test_app;
    use crate::api::server::tests::ErrorResponse;
    use axum_test::{TestResponse, TestServer};
    use serde::Deserialize;
    use serde_json::json;

    #[derive(Debug, Deserialize)]
    struct ResponseBody {
        token: String,
    }

    #[tokio::test]
    async fn login_success() {
        let server = TestServer::new(new_test_app()).unwrap();

        // Get the request.
        let response: TestResponse = server
            .post(&"/login")
            .json(&json!({
                "username": "admin",
                "password": "changeme",
            }))
            .await;

        let response_body: ResponseBody = response.json::<ResponseBody>();
        assert!(!response_body.token.is_empty(), "Token key is missing in the response");
    }

    #[tokio::test]
    async fn login_fail() {
        let server = TestServer::new(new_test_app()).unwrap();

        // Get the request.
        let response: TestResponse = server
            .post(&"/login")
            .json(&json!({
                "username": "coucou",
                "password": "changeme",
            }))
            .await;

        let response_body: ErrorResponse = response.json::<ErrorResponse>();
        assert!(response_body.errors.contains(&"Invalid credentials".to_string()));
    }
}
