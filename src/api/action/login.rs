use crate::api::server::Db;
use crate::api::validation::problem_response;
use crate::models::users as users_model;
use axum::extract::State;
use axum::response::Response;
use axum::{Json, http::StatusCode, response::IntoResponse};
use rand::Rng;
use rand::distr::Alphanumeric;
use rand::rng;
use serde::{Deserialize, Serialize};
use serde_json::json;

pub(crate) async fn login(State(pool): State<Db>, Json(input): Json<LoginInput>) -> Response {
    debug!("Login attempt");

    let option = users_model::find_by_username(&pool, &input.username).await;

    match option {
        Ok(Some(mut user)) => {
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

            if user.token.is_empty() {
                user.token = generate_token();
            }

            let token: String = user.token.clone();

            if users_model::login(&pool, user).await.is_err() {
                return problem_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Internal Server Error",
                    "failed to record login",
                );
            }

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

fn generate_token() -> String {
    format!(
        "tk_{}",
        rng()
            .sample_iter(&Alphanumeric)
            .take(64)
            .map(char::from)
            .collect::<String>()
    )
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

    #[tokio::test]
    async fn login_success() {
        let server = TestServer::new(new_test_app().await).unwrap();

        let response: TestResponse = server
            .post("/login")
            .json(&json!({
                "username": "admin",
                "password": "changeme",
            }))
            .await;

        let response_body: ResponseBody = response.json::<ResponseBody>();
        assert!(
            !response_body.token.is_empty(),
            "Token key is missing in the response"
        );
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
