use chrono::{DateTime, Utc};
use uuid::Uuid;

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json
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
        "docker"  => Ok(()),
        _ => Err(ValidationError::new("invalid runtime values use [docker]")),
    }
}


#[derive(Serialize, Deserialize, Debug, Clone, Validate)]
pub struct Volume {
    pub source: String,
    pub destination: String,
    #[validate(custom = "validate_driver")]
    pub driver: String,
    #[validate(custom = "validate_permission")]
    pub permission: String,
}

fn validate_driver(driver: &str) -> Result<(), ValidationError> {
    match driver {
        "local" | "nfs" => Ok(()),
        _ => Err(ValidationError::new("invalid driver, use [local, nfs]")),
    }
}

fn validate_permission(permission: &str) -> Result<(), ValidationError> {
    match permission {
        "ro" | "rw" => Ok(()),
        _ => Err(ValidationError::new("invalid permission, use [ro, rw]")),
    }
}

#[derive(Deserialize, Serialize, Debug, Clone, Validate)]
pub(crate) struct DeploymentInput {
    name: String,
    #[validate(custom = "validate_runtime")]
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
    #[validate]
    volumes: Vec<Volume>
}

#[derive(Deserialize, Debug)]
pub(crate) struct QueryParameters {
    force: Option<bool>
}

#[derive(Serialize, Deserialize, Debug)]
struct Message {
    message: String
}

pub(crate) async fn create(
    query_parameters: Query<QueryParameters>,
    State(connexion): State<Db>,
    _user: User,
    Json(input): Json<DeploymentInput>,
) -> impl IntoResponse {
    let mut filters = Vec::new();
    filters.push(input.namespace.clone());
    filters.push(input.name.clone());

    let guard = connexion.lock().await;
    let option = deployments::find_one_by_filters(&guard, filters);
    let config = option.as_ref().unwrap();

    match input.validate() {
        Ok(()) => {

            // deployment found
            if config.is_some() {
                info!("Found deployment");
                let mut deployment = config.clone().unwrap();

                //@todo: implement reel deployment diff
                if input.image.to_string() != deployment.image || input.replicas != deployment.replicas || query_parameters.force.is_some() {
                    info!("force update");

                    deployment.status = "deleted".to_string();
                    deployments::update(&guard, &deployment);

                    deployment.id = Uuid::new_v4().to_string();

                    deployment.replicas = input.replicas;
                    deployment.image = input.image.clone();
                    deployment.labels = input.labels;
                    deployment.secrets = input.secrets;
                    deployment.restart_count = 0;
                    deployment.status = "creating".to_string();
                    deployments::create(&guard, &deployment);
                }

                let deployment_output = DeploymentOutput::from_to_model(deployment);

                (StatusCode::CREATED, Json(deployment_output)).into_response()

            }  else {
                info!("Deployment not found, create a new one");

                let utc: DateTime<Utc> = Utc::now();
                let volumes = serde_json::to_string(&input.volumes).unwrap();

                let deployment = deployments::Deployment {
                    id: Uuid::new_v4().to_string(),
                    name: input.name.clone(),
                    runtime: input.runtime.clone(),
                    namespace: input.namespace.clone(),
                    kind: String::from("worker"),
                    image: input.image.clone(),
                    config: input.config.clone(),
                    status: "creating".to_string(),
                    created_at: utc.to_string(),
                    updated_at: None,
                    labels: input.labels,
                    secrets: input.secrets,
                    replicas: input.replicas,
                    instances: [].to_vec(),
                    restart_count: 0,
                    volumes: volumes
                };

                deployments::create(&guard, &deployment);

                let deployment_output = DeploymentOutput::from_to_model(deployment);

                (StatusCode::CREATED, Json(deployment_output)).into_response()
            }
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
            .add_header("Authorization".parse().unwrap(), format!("Bearer {}", token).parse().unwrap())
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
            .add_header("Authorization".parse().unwrap(), format!("Bearer {}", token).parse().unwrap())
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
            .add_header("Authorization".parse().unwrap(), format!("Bearer {}", token).parse().unwrap())
            .json(&json!({
                "runtime": "docker",
                "name": "nginx",
                "namespace": "ring",
                "image": "nginx:latest",
                "volumes": [
                    {
                        "source": "/var/run/docker.sock",
                        "destination": "/var/run/docker.sock",
                        "driver": "local",
                        "permission": "ro"
                    },
                    {
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
            .add_header("Authorization".parse().unwrap(), format!("Bearer {}", token).parse().unwrap())
            .json(&json!({
            "runtime": "docker",
            "name": "nginx",
            "namespace": "ring",
            "image": "nginx:latest",
            "volumes": [
                {
                    "source": "/var/run/docker.sock",
                    "destination": "/var/run/docker.sock",
                    "driver": "local",
                    "permission": "invalid_permission"  // Permission invalide
                }
            ]
        }))
            .await;

        assert_eq!(response.status_code(), StatusCode::BAD_REQUEST);

        let error_body: Message = response.json();
        assert!(error_body.message.contains("invalid permission"));
    }

    #[tokio::test]
    async fn create_with_invalid_volume_driver() {
        let app = new_test_app();
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post(&"/deployments")
            .add_header("Authorization".parse().unwrap(), format!("Bearer {}", token).parse().unwrap())
            .json(&json!({
            "runtime": "docker",
            "name": "nginx",
            "namespace": "ring",
            "image": "nginx:latest",
            "volumes": [
                {
                    "source": "/var/run/docker.sock",
                    "destination": "/var/run/docker.sock",
                    "driver": "invalid_driver",  // Driver invalide
                    "permission": "ro"
                }
            ]
        }))
            .await;

        assert_eq!(response.status_code(), StatusCode::BAD_REQUEST);

        let error_body: Message = response.json();
        assert!(error_body.message.contains("invalid driver"));
    }
}
