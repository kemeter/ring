use axum::{
    extract::{State},
    http::StatusCode,
    response::IntoResponse,
    Json
};
use serde::{Serialize, Deserialize};
use argon2::{self, Config as Argon2Config};
use crate::api::server::Db;
use crate::models::users as users_model;
use crate::models::users::User;
use crate::api::dto::user::UserOutput;
use crate::config::config::{Config};

pub(crate) async fn create(
    State(connexion): State<Db>,
    State(configuration): State<Config>,
    _user: User,
    Json(input): Json<UserInput>,
) -> impl IntoResponse {
    let guard = connexion.lock().await;
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
        input.password.as_bytes(),
        configuration.user.salt.as_bytes(),
        &argon2_config
    ).unwrap();

    users_model::create(&guard, &input.username, &password_hash);
    let option = users_model::find_by_username(&guard, &input.username);
    let user = option.as_ref().unwrap();

    let member = user.clone().unwrap();

    let output = UserOutput {
        id: member.id,
        username: member.username,
        created_at: member.created_at,
        updated_at: member.updated_at,
        status: member.status,
        login_at: member.login_at
    };

    (StatusCode::CREATED, Json(output))
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub(crate) struct UserInput {
    username: String,
    password: String
}

#[cfg(test)]
mod tests {
    use axum_test::{TestResponse, TestServer};
    use http::StatusCode;
    use serde_json::json;
    use crate::api::server::tests::{login, new_test_app};

    #[tokio::test]
    async fn create() {
        let app = new_test_app();
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post(&"/users")
            .add_header("Authorization".parse().unwrap(), format!("Bearer {}", token).parse().unwrap())
            .json(&json!({
                "username": "ring",
                "password": "ring"
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::CREATED);
    }
}
