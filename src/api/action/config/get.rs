use axum::extract::State;
use axum::{Json, extract::Path, response::IntoResponse};

use crate::api::dto::config::ConfigOutput;
use crate::api::server::Db;
use crate::models::config;
use crate::models::users::User;
use axum::http::StatusCode;

pub(crate) async fn get(
    Path(id): Path<String>,
    _user: User,
    State(pool): State<Db>,
) -> impl IntoResponse {
    match config::find(&pool, &id).await {
        Ok(Some(deployment)) => {
            let output = ConfigOutput::from_to_model(deployment);
            Json(output).into_response()
        }
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use crate::api::server::tests::login;
    use crate::api::server::tests::new_test_app;
    use axum::http::StatusCode;
    use axum_test::TestServer;

    #[tokio::test]
    async fn not_fount() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();
        let response = server
            .get("/configs/1")
            .add_header("Authorization", format!("Bearer {}", token))
            .await;

        assert_eq!(response.status_code(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn get() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();
        let response = server
            .get("/configs/cde7806a-21af-473b-968b-08addc7bf0ba")
            .add_header("Authorization", format!("Bearer {}", token))
            .await;

        assert_eq!(response.status_code(), StatusCode::OK);
    }
}
