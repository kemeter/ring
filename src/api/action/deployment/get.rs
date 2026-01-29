use axum::{
    extract::{Path},
    response::IntoResponse,
    Json
};
use axum::extract::State;

use crate::api::server::Db;
use crate::models::deployments;
use crate::api::dto::deployment::DeploymentOutput;
use crate::runtime::runtime::Runtime;
use crate::models::users::User;
use axum::http::StatusCode;

pub(crate) async fn get(
    Path(id): Path<String>,
    _user: User,
    State(connexion): State<Db>,
) -> impl IntoResponse {
    let guard = connexion.lock().await;

    match deployments::find(&guard, id.clone()) {
        Ok(Some(deployment)) => {
            let runtime = Runtime::new(deployment.clone());
            let instances = runtime.list_instances().await;

            let mut output = DeploymentOutput::from_to_model(deployment);
            output.instances = instances;

            Json(output).into_response()
        }
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response()
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
            .get("/deployments/1")
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
            .get("/deployments/658c0199-85a2-49da-86d6-1ecd2e427118")
            .add_header("Authorization", format!("Bearer {}", token))
            .await;

        assert_eq!(response.status_code(), StatusCode::OK);
    }
}