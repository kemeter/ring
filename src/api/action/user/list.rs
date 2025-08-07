use axum::{
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use axum::extract::State;
use serde_json::json;

use crate::api::server::Db;
use crate::models::users as users_model;
use crate::api::dto::user::UserOutput;
use crate::models::users::User;

pub(crate) async fn list(
    State(connexion): State<Db>,
    _user: User
) -> Result<Json<Vec<UserOutput>>, (StatusCode, Json<serde_json::Value>)> {

    let mut users: Vec<UserOutput> = Vec::new();
    let guard = connexion.lock().await;

    let list_users = users_model::find_all(guard).map_err(|_| (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "errors": ["Internal server error"] }))
    ))?;

    for user in list_users.into_iter() {
        let output = UserOutput {
            id: user.id,
            username: user.username,
            created_at: user.created_at,
            updated_at: user.updated_at,
            status: user.status,
            login_at: user.login_at
        };

        users.push(output);
    }

    Ok(Json(users))
}

#[cfg(test)]
mod tests {
    use axum_test::{TestResponse, TestServer};
    use http::StatusCode;
    use crate::api::server::tests::{login, new_test_app};

    #[tokio::test]
    async fn create() {
        let app = new_test_app();
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .get(&"/users")
            .add_header("Authorization", format!("Bearer {}", token))
            .await;

        assert_eq!(response.status_code(), StatusCode::OK);
        let users = response.json::<Vec<serde_json::Value>>();
        assert_eq!(users.len(), 2);
    }
}
