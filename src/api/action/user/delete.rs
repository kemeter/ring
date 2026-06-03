use axum::extract::State;
use axum::response::Response;
use axum::{extract::Path, http::StatusCode, response::IntoResponse};

use crate::api::auth::Auth;
use crate::api::server::Db;
use crate::models::users;

// Scope (`users:write`) is enforced centrally by the auth middleware; the
// admin/self-deletion rules below are an additional, finer-grained check.
pub(crate) async fn delete(Path(id): Path<String>, auth: Auth, State(pool): State<Db>) -> Response {
    // Self-deletion is never allowed (an operator locking themselves out).
    // Deleting *another* account is an admin-only action; without this any
    // authenticated user could delete every other account (IDOR / DoS).
    if auth.user.id == id || !auth.user.is_admin() {
        return StatusCode::FORBIDDEN.into_response();
    }

    let option = users::find(&pool, &id).await;

    match option {
        Ok(Some(user)) => {
            if users::delete(&pool, &user).await.is_err() {
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }

            StatusCode::NO_CONTENT.into_response()
        }
        Ok(None) => StatusCode::NOT_FOUND.into_response(),

        Err(_) => StatusCode::NO_CONTENT.into_response(),
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

    #[tokio::test]
    async fn non_admin_cannot_delete_another_user() {
        // IDOR regression: john.doe (role=user) must NOT be able to delete
        // the admin account.
        let app = new_test_app().await;
        let token = login(app.clone(), "john.doe", "changeme").await;
        let server = TestServer::new(app).unwrap();
        let response = server
            .delete("/users/5b5c370a-cdbf-4fa4-826e-1eea4d8f7d47") // admin's id
            .add_header("Authorization", format!("Bearer {}", token))
            .await;

        assert_eq!(response.status_code(), StatusCode::FORBIDDEN);
    }
}
