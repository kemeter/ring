use crate::api::dto::user::UserOutput;
use crate::models::users::User;
use axum::{response::IntoResponse, Json};

pub(crate) async fn me(user: User) -> impl IntoResponse {
    let output = UserOutput {
        id: user.id,
        username: user.username,
        created_at: user.created_at,
        updated_at: user.updated_at,
        status: user.status,
        login_at: user.login_at,
    };

    Json(output)
}

#[cfg(test)]
mod tests {
    use axum_test::TestServer;
    use axum::http::StatusCode;
    use crate::api::server::tests::new_test_app;
    use crate::api::server::tests::login;

    #[tokio::test]
    async fn me() {
        let app = new_test_app();
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();
        let response = server
            .get("/users/me")
            .add_header("Authorization".parse().unwrap(), format!("Bearer {}", token).parse().unwrap())
            .await;

        assert_eq!(response.status_code(), StatusCode::OK);

        let user = response.json::<serde_json::Value>();

        assert_eq!(user["id"], "1c5a5fe9-84e0-4a18-821e-8058232c2c23");
        assert_eq!(user["username"], "admin");
        assert_eq!(user["status"], "active");
    }
}
