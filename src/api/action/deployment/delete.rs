use axum::{
    extract::{Path},
    http::StatusCode,
    response::IntoResponse
};
use axum::extract::State;

use crate::api::server::Db;
use crate::models::deployments;
use crate::models::users::User;

pub(crate) async fn delete(
    Path(id): Path<String>,
    State(connexion): State<Db>,
    _user: User
) -> impl IntoResponse {
    let guard = connexion.lock().await;
    let option = deployments::find(&guard, id);

    match option {
        Ok(Some(mut deployment)) => {
            deployment.status = "deleted".to_string();
            deployments::update(&guard, &deployment);

            StatusCode::NO_CONTENT
        }
        Ok(None) => {
            StatusCode::NOT_FOUND
        }

        Err(_) => {
            StatusCode::NO_CONTENT
        }
    }
}

#[cfg(test)]
mod tests{
    use super::*;
    use axum_test::{TestResponse, TestServer};
    use crate::api::server::tests::{login, new_test_app};

    #[tokio::test]
    async fn delete() {
        let app = new_test_app();

        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .delete(&"/deployments/658c0199-85a2-49da-86d6-1ecd2e427118")
            .add_header("Authorization", format!("Bearer {}", token))
            .await;

        assert_eq!(response.status_code(), StatusCode::NO_CONTENT);

        let response: TestResponse = server
            .get(&"/deployments/658c0199-85a2-49da-86d6-1ecd2e427118")
            .add_header("Authorization", format!("Bearer {}", token))
            .await;

        assert_eq!(response.status_code(), StatusCode::OK);
        let deployment = response.json::<serde_json::Value>();

        assert_eq!(deployment["status"], "deleted");
    }
}