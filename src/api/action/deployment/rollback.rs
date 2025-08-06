use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Serialize, Deserialize};
use log::info;

use crate::api::server::Db;
use crate::models::{deployments, deployment_event};
use crate::models::users::User;

#[derive(Serialize, Deserialize, Debug)]
struct RollbackResponse {
    message: String,
    previous_deployment_id: Option<String>,
}

#[derive(Serialize, Deserialize, Debug)]
struct Message {
    message: String,
}

pub(crate) async fn rollback(
    State(connexion): State<Db>,
    _user: User,
    Path(deployment_id): Path<String>,
) -> impl IntoResponse {
    let guard = connexion.lock().await;
    
    match deployments::rollback_to_predecessor(&guard, &deployment_id) {
        Ok(Some(predecessor_id)) => {
            info!("Successfully rolled back deployment {} to predecessor {}", deployment_id, predecessor_id);
            
            // Log rollback event
            let _ = deployment_event::log_event(
                &guard,
                predecessor_id.clone(),
                "info",
                format!("Deployment rolled back from failed deployment {}", deployment_id),
                "api",
                Some("DeploymentRollback")
            );
            
            let response = RollbackResponse {
                message: "Deployment rolled back successfully".to_string(),
                previous_deployment_id: Some(predecessor_id),
            };
            (StatusCode::OK, Json(response)).into_response()
        }
        Ok(None) => {
            let message = Message { 
                message: "No predecessor deployment found or rollback not possible".to_string() 
            };
            (StatusCode::BAD_REQUEST, Json(message)).into_response()
        }
        Err(e) => {
            let message = Message { 
                message: format!("Rollback failed: {}", e) 
            };
            (StatusCode::INTERNAL_SERVER_ERROR, Json(message)).into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum_test::{TestResponse, TestServer};
    use serde_json::json;
    use crate::api::server::tests::{login, new_test_app};
    use uuid::Uuid;

    #[tokio::test]
    async fn rollback_nonexistent_deployment() {
        let app = new_test_app();
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let fake_id = Uuid::new_v4().to_string();
        let response: TestResponse = server
            .post(&format!("/deployments/{}/rollback", fake_id))
            .add_header("Authorization", format!("Bearer {}", token))
            .await;

        assert_eq!(response.status_code(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn rollback_without_auth() {
        let app = new_test_app();
        let server = TestServer::new(app).unwrap();

        let fake_id = Uuid::new_v4().to_string();
        let response: TestResponse = server
            .post(&format!("/deployments/{}/rollback", fake_id))
            .await;

        assert_eq!(response.status_code(), StatusCode::UNAUTHORIZED);
    }
}