use axum::extract::State;
use axum::{Json, extract::Path, response::IntoResponse};

use crate::api::dto::deployment::DeploymentOutput;
use crate::api::server::{Db, RuntimeMap};
use crate::models::deployments;
use crate::models::users::User;
use axum::http::StatusCode;

pub(crate) async fn get(
    Path(id): Path<String>,
    _user: User,
    State(pool): State<Db>,
    State(runtimes): State<RuntimeMap>,
) -> impl IntoResponse {
    match deployments::find(&pool, &id).await {
        Ok(Some(deployment)) => {
            let mut output = DeploymentOutput::from_to_model(deployment.clone());
            if let Some(rt) = runtimes.get(&deployment.runtime) {
                output.instances = rt.list_instances(deployment.id, "running").await;
            }
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
            .get("/deployments/1")
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
            .get("/deployments/658c0199-85a2-49da-86d6-1ecd2e427118")
            .add_header("Authorization", format!("Bearer {}", token))
            .await;

        assert_eq!(response.status_code(), StatusCode::OK);
    }

    #[tokio::test]
    async fn get_deployment_with_image_digest() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();
        let response = server
            .get("/deployments/a71f2492-b7c5-42ef-98f8-4hg2527gh451")
            .add_header("Authorization", format!("Bearer {}", token))
            .await;

        assert_eq!(response.status_code(), StatusCode::OK);
        let body: serde_json::Value = response.json();
        assert_eq!(body["image_digest"], "sha256:abc123def456789");
    }

    #[tokio::test]
    async fn get_deployment_without_image_digest() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();
        let response = server
            .get("/deployments/658c0199-85a2-49da-86d6-1ecd2e427118")
            .add_header("Authorization", format!("Bearer {}", token))
            .await;

        assert_eq!(response.status_code(), StatusCode::OK);
        let body: serde_json::Value = response.json();
        assert!(body.get("image_digest").is_none() || body["image_digest"].is_null());
    }
}
