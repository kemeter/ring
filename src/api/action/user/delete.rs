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
        Ok(Some(user)) => match users::delete(&pool, &user).await {
            Ok(true) => StatusCode::NO_CONTENT.into_response(),
            // Refused: removing this account would leave no admin, and so no
            // way back into the instance through the API.
            Ok(false) => StatusCode::CONFLICT.into_response(),
            Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        },
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
    async fn deleting_an_admin_is_allowed_only_while_another_remains() {
        // The API forbids self-deletion, so the last-admin guard is reached by
        // deleting *another* admin. With two admins the delete succeeds; the
        // survivor may then not be deleted, which is what keeps an instance
        // from ending up with no administrator and no way back in.
        let (pool, app) = crate::api::server::tests::new_test_app_with_pool().await;
        sqlx::query(
            "INSERT INTO user (id, created_at, status, role, username, password) VALUES (?, datetime(), 'active', 'admin', 'second.admin', ?)",
        )
        .bind("aa11bb22-cc33-dd44-ee55-ff6677889900")
        .bind("$argon2id$v=19$m=65536,t=2,p=4$Y2hhbmdlbWU$HxyGA81ORfjb63QVOi3+t/eBaFPmdSbf4OZc4pBG8DM")
        .execute(&pool)
        .await
        .unwrap();

        let token = login(app.clone(), "second.admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        // Two admins: deleting the seed admin leaves the caller, so it is fine.
        let first = server
            .delete("/users/1c5a5fe9-84e0-4a18-821e-8058232c2c23")
            .add_header("Authorization", format!("Bearer {}", token))
            .await;
        assert_eq!(first.status_code(), StatusCode::NO_CONTENT);

        // One admin left: the model guard refuses to remove it.
        let last = crate::models::users::find(&pool, "aa11bb22-cc33-dd44-ee55-ff6677889900")
            .await
            .unwrap()
            .unwrap();
        assert!(
            !crate::models::users::delete(&pool, &last).await.unwrap(),
            "deleting the last admin must be refused"
        );
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
