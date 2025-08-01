use std::borrow::Cow;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    Json,
};

use serde::{Serialize, Deserialize};
use std::collections::HashMap;
use validator::{Validate, ValidationError};

use crate::api::server::Db;
use crate::models::deployments;
use crate::api::dto::deployment::DeploymentOutput;
use crate::models::deployments::DeploymentConfig;
use crate::models::users::User;

fn default_replicas() -> u32 { 1 }

fn validate_runtime(runtime: &str) -> Result<(), ValidationError> {
    match runtime {
        "docker" => Ok(()),
        _ => Err(ValidationError::new("invalid runtime values use [docker]")),
    }
}

fn validate_kind(kind: &str) -> Result<(), ValidationError> {
    match kind {
        "job" => Ok(()),
        "worker" => Ok(()),
        _ => Err(ValidationError::new("invalid runtime values use [docker]")),
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "lowercase")]
pub enum VolumeType {
    Bind,
    Config,
    Volume,
}

impl Default for VolumeType {
    fn default() -> Self {
        VolumeType::Bind
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "lowercase")]
pub enum Driver {
    Local,
    Nfs,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "lowercase")]
pub enum Permission {
    Ro,
    Rw,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Volume {
    pub r#type: VolumeType,

    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub source: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub key: Option<String>,

    pub destination: String,
    pub driver: Driver,
    pub permission: Permission,
}

impl Validate for Volume {
    fn validate(&self) -> Result<(), validator::ValidationErrors> {
        let mut errors = validator::ValidationErrors::new();

        if self.destination.is_empty() {
            errors.add("destination", ValidationError::new("destination cannot be empty"));
        }

        match self.r#type {
            VolumeType::Bind => {
                match &self.source {
                    None => {
                        errors.add("source", ValidationError::new("source is required for bind volumes"));
                    }
                    Some(source) if source.is_empty() => {
                        errors.add("source", ValidationError::new("source cannot be empty"));
                    }
                    _ => {}
                }
            }
            VolumeType::Config => {
                let fields_to_validate = [
                    (&self.source, "source", "source"),
                    (&self.key, "key", "key"),
                ];

                for (field, field_name, error_prefix) in fields_to_validate.iter() {
                    match field {
                        None => {
                            let message = format!("{} is required for config volumes", error_prefix);
                            let error = ValidationError {
                                code: Cow::from("required"),
                                message: Some(Cow::Owned(message)),
                                params: HashMap::new(),
                            };
                            errors.add(field_name, error);
                        }
                        Some(value) if value.is_empty() => {
                            let message = format!("{} cannot be empty", error_prefix);
                            let error = ValidationError {
                                code: Cow::from("empty"),
                                message: Some(Cow::Owned(message)),
                                params: HashMap::new(),
                            };
                            errors.add(field_name, error);
                        }
                        _ => {}
                    }
                }

                if !matches!(self.permission, Permission::Ro) {
                    let error = ValidationError {
                        code: Cow::from("invalid_permission"),
                        message: Some(Cow::from("config volumes must be read-only (ro)")),
                        params: HashMap::new(),
                    };
                    errors.add("permission", error);
                }
            }
            VolumeType::Volume => {
                match &self.source {
                    None => {
                        errors.add("source", ValidationError::new("source is required for named volumes"));
                    }
                    Some(source) if source.is_empty() => {
                        errors.add("source", ValidationError::new("source cannot be empty"));
                    }
                    _ => {}
                }
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "lowercase")]
pub enum DeploymentKind {
    Worker,
    Job,
}

impl Default for DeploymentKind {
    fn default() -> Self {
        DeploymentKind::Worker
    }
}

#[derive(Deserialize, Serialize, Debug, Clone, Validate)]
pub(crate) struct DeploymentInput {
    #[serde(default)]
    kind: DeploymentKind,
    name: String,
    #[validate(custom(function = "validate_runtime"))]
    runtime: String,
    namespace: String,
    image: String,
    config: Option<DeploymentConfig>,
    #[serde(default = "default_replicas")]
    replicas: u32,
    #[serde(default)]
    labels: HashMap<String, String>,
    #[serde(default)]
    secrets: HashMap<String, String>,
    #[serde(default)]
    #[validate(nested)]
    volumes: Vec<Volume>,
    #[serde(default)]
    command: Vec<String>
}

#[derive(Serialize, Deserialize, Debug)]
struct Message {
    message: String,
}

pub(crate) async fn create(
    State(connexion): State<Db>,
    _user: User,
    Json(input): Json<DeploymentInput>,
) -> impl IntoResponse {
    
    let mut filters = Vec::new();
    filters.push(input.namespace.clone());
    filters.push(input.name.clone());

    match input.validate() {
        Ok(()) => {
            let guard = connexion.lock().await;
            let active_deployments = deployments::find_active_by_namespace_name(
                &guard,
                input.namespace.clone(),
                input.name.clone(),
            );

            match active_deployments {
                Ok(deployments_list) => {
                    if !deployments_list.is_empty() {
                        info!("Found {} active deployments with the same namespace and name", deployments_list.len());

                        for mut deployment in deployments_list {
                            deployment.status = "deleted".to_string();
                            deployment.updated_at = Some(Utc::now().to_string());
                            deployments::update(&guard, &deployment);
                        }
                    }
                }
                Err(e) => {
                    let message = Message { message: format!("Database error: {}", e.to_string()) };
                    return (StatusCode::INTERNAL_SERVER_ERROR, Json(message)).into_response();
                }
            }

            let utc: DateTime<Utc> = Utc::now();

            // Serialize volumes with error handling
            let volumes = match serde_json::to_string(&input.volumes) {
                Ok(json_str) => json_str,
                Err(e) => {
                    let message = Message { message: format!("Volume serialization error: {}", e) };
                    return (StatusCode::INTERNAL_SERVER_ERROR, Json(message)).into_response();
                }
            };

            let deployment = deployments::Deployment {
                id: Uuid::new_v4().to_string(),
                name: input.name.clone(),
                runtime: input.runtime.clone(),
                namespace: input.namespace.clone(),
                kind: match input.kind {
                    DeploymentKind::Worker => "worker".to_string(),
                    DeploymentKind::Job => "job".to_string(),
                },
                image: input.image.clone(),
                config: input.config.clone(),
                status: "creating".to_string(),
                created_at: utc.to_string(),
                updated_at: None,
                labels: input.labels,
                secrets: input.secrets,
                replicas: input.replicas,
                command: input.command,
                instances: [].to_vec(),
                restart_count: 0,
                volumes: volumes,
            };

            deployments::create(&guard, &deployment);

            let deployment_output = DeploymentOutput::from_to_model(deployment);

            (StatusCode::CREATED, Json(deployment_output)).into_response()
        }
        Err(e) => {
            let message = Message { message: e.to_string() };
            (StatusCode::BAD_REQUEST, Json(message)).into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum_test::{TestResponse, TestServer};
    use serde_json::json;
    use crate::api::server::tests::{login, new_test_app};

    #[tokio::test]
    async fn create_with_invalid_runtime() {
        let app = new_test_app();
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post(&"/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "runtime": "null",
                "name": "nginx",
                "namespace": "ring",
                "image": "nginx:latest"
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn create_with_without_auth() {
        let app = new_test_app();
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post(&"/deployments")
            .json(&json!({
                "runtime": "docker",
                "name": "coucou",
                "namespace": "ring",
                "image": "nginx:latest"
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn create() {
        let app = new_test_app();
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post(&"/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "runtime": "docker",
                "name": "nginx",
                "namespace": "ring",
                "image": "nginx:latest"
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn create_with_volumes() {
        let app = new_test_app();
        let token = login(app.clone(), "john.doe", "john.doe").await;
        dbg!(token.clone());
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post(&"/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "runtime": "docker",
                "name": "nginx",
                "namespace": "ring",
                "image": "nginx:latest",
                "volumes": [
                    {
                        "type": "bind",
                        "source": "/var/run/docker.sock",
                        "destination": "/var/run/docker.sock",
                        "driver": "local",
                        "permission": "ro"
                    },
                    {
                        "type": "bind",
                        "source": "toto",
                        "destination": "/opt/toto",
                        "driver": "local",
                        "permission": "ro"
                    }
                ]
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn create_with_invalid_volume_permission() {
        let app = new_test_app();
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post(&"/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
            "runtime": "docker",
            "name": "nginx",
            "namespace": "ring",
            "image": "nginx:latest",
            "volumes": [
                {
                    "type": "bind",
                    "source": "/var/run/docker.sock",
                    "destination": "/var/run/docker.sock",
                    "driver": "local",
                    "permission": "invalid_permission"  // Invalid permission
                }
            ]
        }))
            .await;

        assert_eq!(response.status_code(), StatusCode::UNPROCESSABLE_ENTITY);

        let error_text = response.text();
        assert!(error_text.contains("unknown variant") || error_text.contains("invalid_permission"));
    }

    #[tokio::test]
    async fn create_with_invalid_volume_driver() {
        let app = new_test_app();
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post(&"/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
            "runtime": "docker",
            "name": "nginx",
            "namespace": "ring",
            "image": "nginx:latest",
            "volumes": [
                {
                    "type": "bind",
                    "source": "/var/run/docker.sock",
                    "destination": "/var/run/docker.sock",
                    "driver": "invalid_driver",  // Invalid driver
                    "permission": "ro"
                }
            ]
        }))
            .await;

        assert_eq!(response.status_code(), StatusCode::UNPROCESSABLE_ENTITY);

        let error_text = response.text();
        assert!(error_text.contains("unknown variant") || error_text.contains("invalid_driver"));
    }

    #[tokio::test]
    async fn create_with_bind_volume_missing_source() {
        let app = new_test_app();
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post(&"/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
            "runtime": "docker",
            "name": "nginx",
            "namespace": "ring",
            "image": "nginx:latest",
            "volumes": [
                {
                    "type": "bind",
                    "destination": "/var/run/docker.sock",
                    "driver": "local",
                    "permission": "ro"
                    // Missing source
                }
            ]
        }))
            .await;

        assert_eq!(response.status_code(), StatusCode::BAD_REQUEST);
        let error_body: Message = response.json();
        assert!(error_body.message.contains("source is required for bind volumes"));
    }

    #[tokio::test]
    async fn create_with_bind_volume_empty_source() {
        let app = new_test_app();
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post(&"/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
            "runtime": "docker",
            "name": "nginx",
            "namespace": "ring",
            "image": "nginx:latest",
            "volumes": [
                {
                    "type": "bind",
                    "source": "",  // Empty source
                    "destination": "/var/run/docker.sock",
                    "driver": "local",
                    "permission": "ro"
                }
            ]
        }))
            .await;

        assert_eq!(response.status_code(), StatusCode::BAD_REQUEST);
        let error_body: Message = response.json();
        assert!(error_body.message.contains("source cannot be empty"));
    }

    #[tokio::test]
    async fn create_with_config_volume_missing_config_reference() {
        let app = new_test_app();
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post(&"/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
            "runtime": "docker",
            "name": "nginx",
            "namespace": "ring",
            "image": "nginx:latest",
            "volumes": [
                {
                    "type": "config",
                    "destination": "/etc/nginx/nginx.conf",
                    "driver": "local",
                    "permission": "ro"
                    // Missing source and key
                }
            ]
        }))
            .await;

        assert_eq!(response.status_code(), StatusCode::BAD_REQUEST);
        let error_body: Message = response.json();
        assert!(error_body.message.contains("source is required for config volumes") ||
            error_body.message.contains("key is required for config volumes"));
    }

    #[tokio::test]
    async fn create_with_config_volume_empty_config_reference() {
        let app = new_test_app();
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post(&"/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
            "runtime": "docker",
            "name": "nginx",
            "namespace": "ring",
            "image": "nginx:latest",
            "volumes": [
                {
                    "type": "config",
                    "source": "",  // Empty source
                    "key": "",   // Empty key
                    "destination": "/etc/nginx/nginx.conf",
                    "driver": "local",
                    "permission": "ro"
                }
            ]
        }))
            .await;

        assert_eq!(response.status_code(), StatusCode::BAD_REQUEST);
        let error_body: Message = response.json();
        assert!(error_body.message.contains("source cannot be empty") ||
            error_body.message.contains("key cannot be empty"));
    }

    #[tokio::test]
    async fn create_with_volume_empty_destination() {
        let app = new_test_app();
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post(&"/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
            "runtime": "docker",
            "name": "nginx",
            "namespace": "ring",
            "image": "nginx:latest",
            "volumes": [
                {
                    "type": "bind",
                    "source": "/var/run/docker.sock",
                    "destination": "",  // Empty destination
                    "driver": "local",
                    "permission": "ro"
                }
            ]
        }))
            .await;

        assert_eq!(response.status_code(), StatusCode::BAD_REQUEST);
        let error_body: Message = response.json();
        assert!(error_body.message.contains("destination cannot be empty"));
    }

    #[tokio::test]
    async fn create_with_invalid_volume_type() {
        let app = new_test_app();
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post(&"/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
            "runtime": "docker",
            "name": "nginx",
            "namespace": "ring",
            "image": "nginx:latest",
            "volumes": [
                {
                    "type": "invalid_type",  // Invalid type
                    "source": "/var/run/docker.sock",
                    "destination": "/var/run/docker.sock",
                    "driver": "local",
                    "permission": "ro"
                }
            ]
        }))
            .await;

        assert_eq!(response.status_code(), StatusCode::UNPROCESSABLE_ENTITY);

        let error_text = response.text();
        assert!(error_text.contains("unknown variant") || error_text.contains("invalid_type"));
    }

    #[tokio::test]
    async fn create_with_valid_bind_volume() {
        let app = new_test_app();
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post(&"/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
            "runtime": "docker",
            "name": "nginx",
            "namespace": "ring",
            "image": "nginx:latest",
            "volumes": [
                {
                    "type": "bind",
                    "source": "/var/run/docker.sock",
                    "destination": "/var/run/docker.sock",
                    "driver": "local",
                    "permission": "ro"
                }
            ]
        }))
            .await;

        assert_eq!(response.status_code(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn create_with_valid_config_volume() {
        let app = new_test_app();
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post(&"/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
            "runtime": "docker",
            "name": "nginx",
            "namespace": "ring",
            "image": "nginx:latest",
            "volumes": [
                {
                    "type": "config",
                    "source": "nginx-config",
                    "key": "nginx.conf",
                    "destination": "/etc/nginx/nginx.conf",
                    "driver": "local",
                    "permission": "ro"
                }
            ]
        }))
            .await;

        assert_eq!(response.status_code(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn create_with_valid_named_volume() {
        let app = new_test_app();
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post(&"/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
            "runtime": "docker",
            "name": "nginx",
            "namespace": "ring",
            "image": "nginx:latest",
            "volumes": [
                {
                    "type": "volume",
                    "source": "my-volume",
                    "destination": "/data",
                    "driver": "local",
                    "permission": "rw"
                }
            ]
        }))
            .await;

        assert_eq!(response.status_code(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn create_with_multiple_volumes_mixed_types() {
        let app = new_test_app();
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post(&"/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
            "runtime": "docker",
            "name": "nginx",
            "namespace": "ring",
            "image": "nginx:latest",
            "volumes": [
                {
                    "type": "bind",
                    "source": "/var/run/docker.sock",
                    "destination": "/var/run/docker.sock",
                    "driver": "local",
                    "permission": "ro"
                },
                {
                    "type": "config",
                    "source": "nginx-config",
                    "key": "nginx.conf",
                    "destination": "/etc/nginx/nginx.conf",
                    "driver": "nfs",
                    "permission": "ro"
                },
                {
                    "type": "volume",
                    "source": "data-volume",
                    "destination": "/data",
                    "driver": "local",
                    "permission": "rw"
                }
            ]
        }))
            .await;

        assert_eq!(response.status_code(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn create_with_multiple_validation_errors() {
        let app = new_test_app();
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post(&"/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
            "runtime": "docker",
            "name": "nginx",
            "namespace": "ring",
            "image": "nginx:latest",
            "volumes": [
                {
                    "type": "bind",
                    // Missing source
                    "destination": "",  // Empty destination
                    "driver": "local",
                    "permission": "ro"
                }
            ]
        }))
            .await;

        assert_eq!(response.status_code(), StatusCode::BAD_REQUEST);
        let error_body: Message = response.json();
        // Should contain both errors
        let message = &error_body.message;
        assert!(message.contains("source") || message.contains("destination"));
    }

    #[tokio::test]
    async fn create_with_config_volume_missing_source_only() {
        let app = new_test_app();
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post(&"/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
            "runtime": "docker",
            "name": "nginx",
            "namespace": "ring",
            "image": "nginx:latest",
            "volumes": [
                {
                    "type": "config",
                    "key": "nginx.conf",  // Only key, missing source
                    "destination": "/etc/nginx/nginx.conf",
                    "driver": "local",
                    "permission": "ro"
                }
            ]
        }))
            .await;

        assert_eq!(response.status_code(), StatusCode::BAD_REQUEST);
        let error_body: Message = response.json();
        assert!(error_body.message.contains("source is required for config volumes"));
    }

    #[tokio::test]
    async fn create_with_config_volume_missing_key_only() {
        let app = new_test_app();
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post(&"/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
            "runtime": "docker",
            "name": "nginx",
            "namespace": "ring",
            "image": "nginx:latest",
            "volumes": [
                {
                    "type": "config",
                    "source": "nginx-config",  // Only source, missing key
                    "destination": "/etc/nginx/nginx.conf",
                    "driver": "local",
                    "permission": "ro"
                }
            ]
        }))
            .await;

        assert_eq!(response.status_code(), StatusCode::BAD_REQUEST);
        let error_body: Message = response.json();
        assert!(error_body.message.contains("key is required for config volumes"));
    }

    #[tokio::test]
    async fn create_with_config_volume_empty_source_only() {
        let app = new_test_app();
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post(&"/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
            "runtime": "docker",
            "name": "nginx",
            "namespace": "ring",
            "image": "nginx:latest",
            "volumes": [
                {
                    "type": "config",
                    "source": "",  // Empty source
                    "key": "nginx.conf",
                    "destination": "/etc/nginx/nginx.conf",
                    "driver": "local",
                    "permission": "ro"
                }
            ]
        }))
            .await;

        assert_eq!(response.status_code(), StatusCode::BAD_REQUEST);
        let error_body: Message = response.json();
        assert!(error_body.message.contains("source cannot be empty"));
    }

    #[tokio::test]
    async fn create_with_config_volume_empty_key_only() {
        let app = new_test_app();
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post(&"/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
            "runtime": "docker",
            "name": "nginx",
            "namespace": "ring",
            "image": "nginx:latest",
            "volumes": [
                {
                    "type": "config",
                    "source": "nginx-config",
                    "key": "",  // Empty key
                    "destination": "/etc/nginx/nginx.conf",
                    "driver": "local",
                    "permission": "ro"
                }
            ]
        }))
            .await;

        assert_eq!(response.status_code(), StatusCode::BAD_REQUEST);
        let error_body: Message = response.json();
        assert!(error_body.message.contains("key cannot be empty"));
    }

    #[tokio::test]
    async fn create_with_named_volume_missing_source() {
        let app = new_test_app();
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post(&"/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
            "runtime": "docker",
            "name": "nginx",
            "namespace": "ring",
            "image": "nginx:latest",
            "volumes": [
                {
                    "type": "volume",
                    "destination": "/data",
                    "driver": "local",
                    "permission": "rw"
                    // Missing source (volume name)
                }
            ]
        }))
            .await;

        assert_eq!(response.status_code(), StatusCode::BAD_REQUEST);
        let error_body: Message = response.json();
        assert!(error_body.message.contains("source is required for named volumes"));
    }

    #[tokio::test]
    async fn create_with_named_volume_empty_source() {
        let app = new_test_app();
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post(&"/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
            "runtime": "docker",
            "name": "nginx",
            "namespace": "ring",
            "image": "nginx:latest",
            "volumes": [
                {
                    "type": "volume",
                    "source": "",  // Empty source
                    "destination": "/data",
                    "driver": "local",
                    "permission": "rw"
                }
            ]
        }))
            .await;

        assert_eq!(response.status_code(), StatusCode::BAD_REQUEST);
        let error_body: Message = response.json();
        assert!(error_body.message.contains("source cannot be empty"));
    }

    #[tokio::test]
    async fn create_with_config_volume_invalid_permission() {
        let app = new_test_app();
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post(&"/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
            "runtime": "docker",
            "name": "nginx",
            "namespace": "ring",
            "image": "nginx:latest",
            "volumes": [
                {
                    "type": "config",
                    "source": "nginx-config",
                    "key": "nginx.conf",
                    "destination": "/etc/nginx/nginx.conf",
                    "driver": "local",
                    "permission": "rw"  // INVALID: config volumes must be read-only
                }
            ]
        }))
            .await;

        assert_eq!(response.status_code(), StatusCode::BAD_REQUEST);
        let error_body: Message = response.json();
        assert!(error_body.message.contains("config volumes must be read-only"));
    }

    #[tokio::test]
    async fn create_worker_with_json_array_command() {
        let app = new_test_app();
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post(&"/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
            "kind": "worker",
            "runtime": "docker",
            "name": "echo-worker",
            "namespace": "test",
            "image": "alpine:latest",
            "command": "[\"sh\", \"-c\", \"while true; do echo 'Worker running'; sleep 30; done\"]",
            "replicas": 2
        }))
            .await;

        assert_eq!(response.status_code(), StatusCode::CREATED);

        let deployment: DeploymentOutput = response.json();
        assert_eq!(deployment.kind, "worker");
        assert_eq!(deployment.replicas, 2);
    }
}