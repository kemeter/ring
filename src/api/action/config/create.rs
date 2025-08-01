use axum::extract::State;
use axum::Json;
use axum::response::IntoResponse;
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use validator::Validate;
use crate::api::server::Db;
use crate::models::config;
use crate::models::users::User;

#[derive(Deserialize, Serialize, Debug, Clone, Validate)]
pub(crate) struct ConfigInput {
    namespace: String,
    name: String,
    data: String,
    #[serde(default)]
    labels: Option<String>,
}

impl ConfigInput {
    fn validate(&self) -> Result<(), validator::ValidationErrors> {
        let errors = validator::ValidationErrors::new();

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

pub(crate) async fn create(
    State(connexion): State<Db>,
    _user: User,
    Json(input): Json<ConfigInput>,
) -> impl IntoResponse {
    match input.validate() {
        Ok(_) => {
            let guard = connexion.lock().await;
            let utc: DateTime<Utc> = Utc::now();

            let config = config::Config {
                id: Uuid::new_v4().to_string(),
                created_at: utc.to_string(),
                updated_at: None,
                namespace: input.namespace,
                name: input.name,
                data: input.data,
                labels: input.labels.unwrap_or_default(),
            };

            match config::create(&guard, config.clone()) {
                Ok(_) => {
                    (StatusCode::CREATED, Json(serde_json::to_value(config).unwrap())).into_response()
                }
                Err(e) => {
                    if e.to_string().contains("UNIQUE constraint failed") {
                        (StatusCode::CONFLICT, Json(serde_json::json!({
                            "error": "Configuration with this name already exists"
                        }))).into_response()
                    } else {
                        (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({
                            "error": "Failed to create configuration"
                        }))).into_response()
                    }
                }
            }
        }
        Err(validation_errors) => {
            (StatusCode::BAD_REQUEST, Json(serde_json::json!({
                "error": "Validation failed",
                "details": validation_errors
            }))).into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use axum_test::TestServer;
    use axum::http::StatusCode;
    use crate::api::server::tests::new_test_app;
    use crate::api::server::tests::login;
    use crate::api::dto::config::ConfigOutput;

    #[tokio::test]
    async fn create_config() {
        let app = new_test_app();
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response = server
            .post("/configs")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&serde_json::json!({
                "namespace": "test",
                "name": "test-config",
                "data": "test data",
                "labels": "test-label"
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::CREATED);
        let config = response.json::<ConfigOutput>();
        assert_eq!(config.namespace, "test");
        assert_eq!(config.name, "test-config");
        assert_eq!(config.data, "test data");
        assert_eq!(config.labels, "test-label");
    }

    #[tokio::test]
    async fn create_duplicate_config() {
        let app = new_test_app();
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        // Create first config
        let response = server
            .post("/configs")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&serde_json::json!({
                "namespace": "test",
                "name": "duplicate-config",
                "data": "test data"
            }))
            .await;
        assert_eq!(response.status_code(), StatusCode::CREATED);

        // Try to create duplicate config
        let response = server
            .post("/configs")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&serde_json::json!({
                "namespace": "test", 
                "name": "duplicate-config",
                "data": "test data"
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn create_invalid_config() {
        let app = new_test_app();
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response = server
            .post("/configs")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&serde_json::json!({
                "name": "test-config",
                "data": "test data"
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::UNPROCESSABLE_ENTITY);
    }
}