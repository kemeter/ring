use axum::extract::{Path, State};
use axum::{response::IntoResponse, Json};
use http::StatusCode;
use serde::Deserialize;
use validator::Validate;

use crate::api::dto::config::ConfigOutput;
use crate::api::server::Db;
use crate::models::config as ConfigModel;
use crate::models::users::User;

#[derive(Deserialize, Debug, Validate)]
pub(crate) struct UpdateConfigRequest {
    #[validate(length(min = 1, max = 255))]
    pub name: Option<String>,
    
    pub data: Option<String>,
    
    #[validate(length(max = 1000))]
    pub labels: Option<String>,
}

pub(crate) async fn update(
    Path(id): Path<String>,
    State(connexion): State<Db>,
    _user: User,
    Json(request): Json<UpdateConfigRequest>,
) -> impl IntoResponse {
    // Validate request
    if let Err(errors) = request.validate() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "Validation failed",
                "details": errors
            }))
        ).into_response();
    }

    // Validate JSON data if provided
    if let Some(ref data) = request.data {
        if serde_json::from_str::<serde_json::Value>(data).is_err() {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "Invalid JSON format in data field"}))
            ).into_response();
        }
    }

    let guard = connexion.lock().await;
    
    // Find existing config
    match ConfigModel::find(&guard, id.clone()) {
        Ok(Some(mut config)) => {
            // Update fields if provided
            if let Some(name) = request.name {
                config.name = name;
            }
            
            if let Some(data) = request.data {
                config.data = data;
            }
            
            if let Some(labels) = request.labels {
                config.labels = labels;
            }
            
            config.updated_at = Some(chrono::Utc::now().to_rfc3339());
            
            match ConfigModel::update(&guard, config.clone()) {
                Ok(_) => {
                    let output = ConfigOutput::from_to_model(config);
                    (StatusCode::OK, Json(output)).into_response()
                },
                Err(_) => {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(serde_json::json!({"error": "Failed to update configuration"}))
                    ).into_response()
                }
            }
        },
        Ok(None) => {
            (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "Configuration not found"}))
            ).into_response()
        },
        Err(_) => {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "Database error"}))
            ).into_response()
        }
    }
}


#[cfg(test)]
mod tests {
    use crate::api::dto::config::ConfigOutput;
    use crate::api::server::tests::login;
    use crate::api::server::tests::new_test_app;
    use axum::http::StatusCode;
    use axum_test::TestServer;
    use serde_json::json;

    #[tokio::test]
    async fn update_config_name() {
        let app = new_test_app();
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();
        
        let response = server
            .put("/configs/cde7806a-21af-473b-968b-08addc7bf0ba")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "name": "updated-config-name"
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::OK);
        
        let config = response.json::<ConfigOutput>();
        assert_eq!(config.name, "updated-config-name");
        assert!(config.updated_at.is_some());
    }

    #[tokio::test]
    async fn update_config_data() {
        let app = new_test_app();
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();
        
        let response = server
            .put("/configs/cde7806a-21af-473b-968b-08addc7bf0ba")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "data": "{\"updated\": true}"
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::OK);
        
        let config = response.json::<ConfigOutput>();
        assert_eq!(config.data, "{\"updated\": true}");
    }

    #[tokio::test]
    async fn update_config_invalid_json_data() {
        let app = new_test_app();
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();
        
        let response = server
            .put("/configs/cde7806a-21af-473b-968b-08addc7bf0ba")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "data": "invalid json"
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn update_nonexistent_config() {
        let app = new_test_app();
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();
        
        let response = server
            .put("/configs/nonexistent")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "name": "new-name"
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn update_config_multiple_fields() {
        let app = new_test_app();
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();
        
        let response = server
            .put("/configs/cde7806a-21af-473b-968b-08addc7bf0ba")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "name": "multi-update",
                "data": "{\"env\": \"production\"}",
                "labels": "{\"team\": \"backend\"}"
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::OK);
        
        let config = response.json::<ConfigOutput>();
        assert_eq!(config.name, "multi-update");
        assert_eq!(config.data, "{\"env\": \"production\"}");
        assert_eq!(config.labels, "{\"team\": \"backend\"}");
    }
}