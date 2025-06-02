use axum::{
    extract::{Path},
    response::IntoResponse,
    Json
};
use axum::extract::State;

use crate::api::server::Db;
use crate::models::config;
use crate::models::users::User;
use axum::http::StatusCode;
use crate::api::dto::config::ConfigOutput;

pub(crate) async fn get(
    Path(id): Path<String>,
    _user: User,
    State(connexion): State<Db>,
) -> impl IntoResponse {
    let guard = connexion.lock().await;

    let option = config::find(&guard, id.clone());
    if let Some(deployment) = option.unwrap() {

        let output = ConfigOutput::from_to_model(deployment);

        Json(output).into_response()
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}

#[cfg(test)]
mod tests {
    use axum_test::TestServer;
    use axum::http::StatusCode;
    use crate::api::server::tests::new_test_app;
    use crate::api::server::tests::login;

    #[tokio::test]
    async fn not_fount() {
        let app = new_test_app();
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
        let app = new_test_app();
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();
        let response = server
            .get("/configs/cde7806a-21af-473b-968b-08addc7bf0ba")
            .add_header("Authorization", format!("Bearer {}", token))
            .await;

        assert_eq!(response.status_code(), StatusCode::OK);
    }
}