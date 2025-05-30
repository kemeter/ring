use axum::extract::{Path, State};
use axum::response::IntoResponse;
use http::StatusCode;

use crate::api::server::{AuthError, Db};
use crate::models::config as ConfigModel;
use crate::models::users::User;

pub(crate) async fn delete(
    Path(id): Path<String>,
    State(connexion): State<Db>,
    _user: User,
) -> Result<impl IntoResponse, AuthError> {

    let guard = connexion.lock().await;
    let result = ConfigModel::delete(&guard, id.clone());
    if let Err(ref err) = result {
        log::error!("Failed to delete configuration with ID {}: {}", id, err);
    }

    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use crate::api::server::tests::{login, new_test_app};
    use axum::http::StatusCode;
    use axum_test::TestServer;

    #[tokio::test]
    async fn delete() {
        let app = new_test_app();
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response = server
            .delete("/configs/cde7806a-21af-473b-968b-08addc7bf0ba")
            .add_header("Authorization", format!("Bearer {}", token))
            .await;

        assert_eq!(response.status_code(), StatusCode::NO_CONTENT);

        let response = server
            .get("/configs/cde7806a-21af-473b-968b-08addc7bf0ba") // Assuming 9999 does not exist
            .add_header("Authorization", format!("Bearer {}", token))
            .await;

        assert_eq!(response.status_code(), StatusCode::NOT_FOUND);

    }
}