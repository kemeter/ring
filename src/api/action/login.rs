use axum::{
    http::StatusCode,
    response::IntoResponse,
    Json
};
use axum::extract::State;
use serde::{Serialize, Deserialize};
use crate::api::server::Db;
use crate::models::users as users_model;
use uuid::Uuid;

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
                Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(HttpResponse {
                    errors: vec!["Internal server error".to_string()],
                    token: "".to_string(),
                })),
            };

            if !matches {
                return error_response("Invalid credentials");
            }

            if user.token.is_empty() {
                user.token = Uuid::new_v4().to_string();
            }

            let output = HttpResponse {
                errors: vec![],
                token: user.token.clone(),
            };

            {
                let guard = connexion.lock().await;
                users_model::login(&guard, user);
            }

            (StatusCode::OK, Json(output))
        }
        Ok(None) => error_response("Invalid credentials"),
        Err(_) => error_response("Internal server error"),
    }
}

fn error_response(message: &str) -> (StatusCode, Json<HttpResponse>) {
    let output = HttpResponse {
        errors: vec![message.to_string()],
        token: "".to_string(),
    };
    (StatusCode::BAD_REQUEST, Json(output))
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
