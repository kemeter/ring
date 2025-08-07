use axum::{
    extract::{Path},
    response::IntoResponse,
    Json
};
use serde::{Serialize, Deserialize};
use serde_json::json;
use argon2::{self, Config as Argon2Config};
use axum::extract::State;
use http::StatusCode;
use crate::api::server::Db;
use crate::models::users as users_model;
use crate::models::users::User;

use crate::config::config::{Config};

pub(crate) async fn update(
    State(connexion): State<Db>,
    State(configuration): State<Config>,
    Path(id): Path<String>,
    _user: User,
    Json(input): Json<UserInput>,
) -> impl IntoResponse {

    let mut user = match {
        let guard = connexion.lock().await;
        users_model::find(&guard, id).ok().flatten()
    } {
        Some(user) => user,
        None => return (StatusCode::NOT_FOUND, "User not found").into_response(),
    };

    if let Some(username) = input.username {
        user.username = username;
    }

    if let Some(password) = input.password {
        let argon2_config = Argon2Config {
            variant: argon2::Variant::Argon2id,
            version: argon2::Version::Version13,
            mem_cost: 65536,
            time_cost: 2,
            lanes: 4,
            secret: &[],
            ad: &[],
            hash_length: 32,
        };

        let password_hash = argon2::hash_encoded(
            password.as_bytes(),
            configuration.user.salt.as_bytes(),
            &argon2_config
        ).unwrap();

        user.password = password_hash;
    }

    let guard = connexion.lock().await;
    if let Err(_) = users_model::update(&guard, &user) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "errors": ["Failed to update user"] }))
        ).into_response();
    }

    return StatusCode::OK.into_response();
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub(crate) struct UserInput {
    username: Option<String>,
    password: Option<String>
}

#[cfg(test)]
mod tests {
    use axum_test::{TestResponse, TestServer};
    use http::StatusCode;
    use serde_json::json;
    use crate::api::server::tests::{login, new_test_app};

    #[tokio::test]
    async fn update_not_found() {
        let app = new_test_app();
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .put(&"/users/non-existent-id")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "username": "newname"
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn update_username() {
        let app = new_test_app();
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .put(&"/users/1c5a5fe9-84e0-4a18-821e-8058232c2c23")
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
        let app = new_test_app();
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .put(&"/users/1c5a5fe9-84e0-4a18-821e-8058232c2c23")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "password": "newpassword"
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::OK);
    }
}
