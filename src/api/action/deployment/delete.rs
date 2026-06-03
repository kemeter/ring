use axum::extract::State;
use axum::{extract::Path, http::StatusCode, response::IntoResponse};

use crate::api::auth::{Auth, require_namespace};
use crate::api::server::Db;
use crate::models::audit_log;
use crate::models::deployments::{self, DeploymentStatus};

pub(crate) async fn delete(
    Path(id): Path<String>,
    State(pool): State<Db>,
    auth: Auth,
) -> impl IntoResponse {
    let option = deployments::find(&pool, &id).await;

    match option {
        Ok(Some(mut deployment)) => {
            // Scope (`deployments:write`) is enforced centrally; the namespace
            // boundary is checked here against the loaded deployment.
            if let Err(resp) = require_namespace(&auth.source, &deployment.namespace) {
                return resp.into_response();
            }
            deployment.status = DeploymentStatus::Deleted;
            match deployments::update(&pool, &deployment).await {
                Ok(_) => {
                    let _ = audit_log::record(
                        &pool,
                        Some(&auth.user.id),
                        "delete",
                        "deployment",
                        &deployment.name,
                        Some(&deployment.namespace),
                    )
                    .await;
                    StatusCode::NO_CONTENT.into_response()
                }
                Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
            }
        }
        Ok(None) => StatusCode::NOT_FOUND.into_response(),

        Err(_) => StatusCode::NO_CONTENT.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::server::tests::{login, new_test_app};
    use axum_test::{TestResponse, TestServer};

    #[tokio::test]
    async fn delete() {
        let app = new_test_app().await;

        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .delete("/deployments/658c0199-85a2-49da-86d6-1ecd2e427118")
            .add_header("Authorization", format!("Bearer {}", token))
            .await;

        assert_eq!(response.status_code(), StatusCode::NO_CONTENT);

        let response: TestResponse = server
            .get("/deployments/658c0199-85a2-49da-86d6-1ecd2e427118")
            .add_header("Authorization", format!("Bearer {}", token))
            .await;

        assert_eq!(response.status_code(), StatusCode::OK);
        let deployment = response.json::<serde_json::Value>();

        assert_eq!(deployment["status"], "deleted");
    }

    #[tokio::test]
    async fn delete_not_found() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .delete("/deployments/00000000-0000-0000-0000-000000000000")
            .add_header("Authorization", format!("Bearer {}", token))
            .await;

        assert_eq!(response.status_code(), StatusCode::NOT_FOUND);
    }
}
