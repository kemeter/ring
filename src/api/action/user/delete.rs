use axum::extract::State;
use axum::{extract::Path, http::StatusCode, response::IntoResponse};

use crate::api::server::Db;
use crate::models::users;
use crate::models::users::User;

pub(crate) async fn delete(
    Path(id): Path<String>,
    current_user: User,
    State(pool): State<Db>,
) -> impl IntoResponse {
    if current_user.id == id {
        return StatusCode::FORBIDDEN;
    }

    let option = users::find(&pool, id).await;

    match option {
        Ok(Some(user)) => {
            if users::delete(&pool, &user).await.is_err() {
                return StatusCode::INTERNAL_SERVER_ERROR;
            }

            StatusCode::NO_CONTENT
        }
        Ok(None) => StatusCode::NOT_FOUND,

        Err(_) => StatusCode::NO_CONTENT,
    }
}

#[cfg(test)]
mod tests {
    use crate::api::server::tests::login;
    use crate::api::server::tests::new_test_app;
    use axum::http::StatusCode;
    use axum_test::TestServer;

    #[tokio::test]
    async fn delete() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();
        let response = server
            .delete("/users/5b5c370a-cdbf-4fa4-826e-1eea4d8f7d47")
            .add_header("Authorization", format!("Bearer {}", token))
            .await;

        assert_eq!(response.status_code(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn cannot_delete_self() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();
        let response = server
            .delete("/users/1c5a5fe9-84e0-4a18-821e-8058232c2c23")
            .add_header("Authorization", format!("Bearer {}", token))
            .await;

        assert_eq!(response.status_code(), StatusCode::FORBIDDEN);
    }
}
