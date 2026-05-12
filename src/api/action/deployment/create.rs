use chrono::{DateTime, Utc};
use std::borrow::Cow;
use uuid::Uuid;

use axum::{
    Json,
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
};

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use validator::{Validate, ValidationError};

use crate::api::dto::deployment::DeploymentOutput;
use crate::api::server::Db;
use crate::models::deployment_event;
use crate::models::deployments;
use crate::models::deployments::{
    DeploymentConfig, DeploymentPort, DeploymentStatus, EnvValue, NetworkConfig, NetworkMode,
    Resource,
};
use crate::models::namespace;
use crate::models::users::User;

fn default_replicas() -> u32 {
    1
}

fn validate_runtime(runtime: &str) -> Result<(), ValidationError> {
    match runtime {
        "docker" | "cloud-hypervisor" => Ok(()),
        _ => Err(ValidationError::new(
            "invalid runtime, supported: [docker, cloud-hypervisor]",
        )),
    }
}

fn validate_network_constraints(input: &DeploymentInput) -> Result<(), String> {
    let Some(network) = &input.network else {
        return Ok(());
    };
    if !matches!(network.mode, NetworkMode::Host) {
        return Ok(());
    }

    if input.runtime != "docker" {
        return Err(format!(
            "network.mode=host is only supported on the docker runtime, got '{}'",
            input.runtime
        ));
    }

    if !input.ports.is_empty() {
        return Err(
            "network.mode=host is incompatible with port mappings: host networking bypasses Docker's port bindings, so `ports` would be silently ignored. Remove `ports` and let the container bind directly on the host."
                .to_string(),
        );
    }

    if input.replicas > 1 {
        return Err(format!(
            "network.mode=host is incompatible with replicas > 1 (got {}): all replicas would compete for the same host ports. Reduce replicas to 1.",
            input.replicas
        ));
    }

    Ok(())
}

fn validate_runtime_constraints(input: &DeploymentInput) -> Result<(), String> {
    if input.runtime == "cloud-hypervisor" {
        // `command` health checks are now supported via the in-guest
        // `ring-agent` daemon (vsock). The guest image must ship the agent
        // listening on AF_VSOCK port 2375 — see the runtime documentation.

        // Reject silently-dropped fields up front so users get a clear error
        // instead of a deployment that runs but ignores half its configuration.
        // (environment is now supported via cloud-init NoCloud — see
        //  src/runtime/cloud_hypervisor/cloud_init.rs. Requires the guest
        //  image to ship cloud-init, which every standard cloud image does.)
        if !input.command.is_empty() {
            return Err(
                "custom commands are not supported on cloud-hypervisor runtime (alpha); the VM boots whatever its image is configured to run".to_string(),
            );
        }

        // CH expects a raw disk image path on the host, not a Docker image
        // reference. Anything that doesn't start with '/' is almost certainly
        // a Docker reference (e.g. `nginx:latest`) — fail early instead of
        // letting it fail much later with a confusing "VM image not found".
        if !input.image.starts_with('/') {
            return Err(format!(
                "cloud-hypervisor runtime expects an absolute path to a raw disk image, got '{}' (Docker image references are not supported)",
                input.image
            ));
        }
    }
    Ok(())
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum VolumeType {
    #[default]
    Bind,
    Config,
    Volume,
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
            errors.add(
                "destination",
                ValidationError::new("destination cannot be empty"),
            );
        }

        match self.r#type {
            VolumeType::Bind => match &self.source {
                None => {
                    errors.add(
                        "source",
                        ValidationError::new("source is required for bind volumes"),
                    );
                }
                Some(source) if source.is_empty() => {
                    errors.add("source", ValidationError::new("source cannot be empty"));
                }
                _ => {}
            },
            VolumeType::Config => {
                let fields_to_validate = [
                    (&self.source, "source", "source"),
                    (&self.key, "key", "key"),
                ];

                for (field, field_name, error_prefix) in fields_to_validate.iter() {
                    match field {
                        None => {
                            let message =
                                format!("{} is required for config volumes", error_prefix);
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
            VolumeType::Volume => match &self.source {
                None => {
                    errors.add(
                        "source",
                        ValidationError::new("source is required for named volumes"),
                    );
                }
                Some(source) if source.is_empty() => {
                    errors.add("source", ValidationError::new("source cannot be empty"));
                }
                _ => {}
            },
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
#[derive(Default)]
pub enum DeploymentKind {
    #[default]
    Worker,
    Job,
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
    environment: HashMap<String, EnvValue>,
    #[serde(default)]
    #[validate(nested)]
    volumes: Vec<Volume>,
    #[serde(default)]
    command: Vec<String>,
    #[serde(default)]
    health_checks: Option<Vec<crate::models::health_check::HealthCheck>>,
    #[serde(default)]
    resources: Option<Resource>,
    #[serde(default)]
    ports: Vec<DeploymentPort>,
    #[serde(default)]
    network: Option<NetworkConfig>,
}

#[derive(Serialize, Deserialize, Debug)]
struct Message {
    message: String,
}

#[derive(Deserialize, Debug, Default)]
pub(crate) struct CreateQueryParams {
    #[serde(default)]
    force: bool,
}

pub(crate) async fn create(
    State(pool): State<Db>,
    _user: User,
    Query(params): Query<CreateQueryParams>,
    Json(input): Json<DeploymentInput>,
) -> impl IntoResponse {
    let mut filters = Vec::new();
    filters.push(input.namespace.clone());
    filters.push(input.name.clone());

    match input.validate() {
        Ok(()) => {
            if let Err(msg) = validate_runtime_constraints(&input) {
                let message = Message { message: msg };
                return (StatusCode::BAD_REQUEST, Json(message)).into_response();
            }

            if let Err(msg) = validate_network_constraints(&input) {
                let message = Message { message: msg };
                return (StatusCode::BAD_REQUEST, Json(message)).into_response();
            }

            // Auto-create namespace if it doesn't exist
            match namespace::find_by_name(&pool, &input.namespace).await {
                Ok(None) => {
                    let new_namespace = namespace::Namespace {
                        id: Uuid::new_v4().to_string(),
                        created_at: Utc::now().to_string(),
                        updated_at: None,
                        name: input.namespace.clone(),
                    };
                    if let Err(e) = namespace::create(&pool, new_namespace).await
                        && !e.to_string().contains("UNIQUE constraint failed")
                    {
                        log::error!("Failed to create namespace '{}': {}", input.namespace, e);
                        let message = Message {
                            message: "Failed to create namespace".to_string(),
                        };
                        return (StatusCode::INTERNAL_SERVER_ERROR, Json(message)).into_response();
                    }
                    info!("Namespace '{}' created automatically", input.namespace);
                }
                Ok(Some(_)) => {}
                Err(e) => {
                    log::error!("Failed to check namespace '{}': {}", input.namespace, e);
                    let message = Message {
                        message: "Internal server error".to_string(),
                    };
                    return (StatusCode::INTERNAL_SERVER_ERROR, Json(message)).into_response();
                }
            }

            let active_deployments =
                deployments::find_active_by_namespace_name(&pool, &input.namespace, &input.name)
                    .await;

            // Determine whether rolling update is possible:
            // - only when there is exactly one active deployment (the current one)
            // - it has health checks configured
            // - --force flag is not set
            let mut rolling_parent_id: Option<String> = None;
            // Captured to log a `ForceReplace` event on the new deployment once
            // it exists. We collect the reason here so the caller of the API
            // gets a clear explanation for why rolling didn't happen, instead
            // of having to compare timestamps across two deployments.
            let mut replaced_deployment_ids: Vec<String> = Vec::new();
            let mut replace_reason: Option<&'static str> = None;

            match active_deployments {
                Ok(deployments_list) => {
                    info!(
                        "Checking for existing deployments: namespace='{}', name='{}' - found: {}",
                        input.namespace,
                        input.name,
                        deployments_list.len()
                    );

                    if !deployments_list.is_empty() {
                        info!(
                            "Found {} active deployments with the same namespace and name",
                            deployments_list.len()
                        );

                        let has_health_checks = input
                            .health_checks
                            .as_ref()
                            .map(|hc| !hc.is_empty())
                            .unwrap_or(false);

                        // Rolling update: keep old deployment running if conditions are met
                        if !params.force && has_health_checks && deployments_list.len() == 1 {
                            let existing = &deployments_list[0];
                            info!(
                                "Rolling update: keeping deployment {} running as parent",
                                existing.id
                            );
                            rolling_parent_id = Some(existing.id.clone());
                        } else {
                            // Immediate replace. Pick the most specific reason so
                            // operators can fix the root cause: `force=true` is a
                            // deliberate caller choice, the others are config gaps.
                            replace_reason = Some(if params.force {
                                "force"
                            } else if !has_health_checks {
                                "no_health_checks"
                            } else {
                                "multiple_active_deployments"
                            });
                            for mut deployment in deployments_list {
                                info!("Marking deployment {} as deleted", deployment.id);
                                replaced_deployment_ids.push(deployment.id.clone());
                                deployment.status = DeploymentStatus::Deleted;
                                deployment.updated_at = Some(Utc::now().to_string());
                                if let Err(e) = deployments::update(&pool, &deployment).await {
                                    log::error!(
                                        "Failed to mark deployment {} as deleted: {}",
                                        deployment.id,
                                        e
                                    );
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    log::error!("Database error while checking active deployments: {}", e);
                    let message = Message {
                        message: "Internal server error".to_string(),
                    };
                    return (StatusCode::INTERNAL_SERVER_ERROR, Json(message)).into_response();
                }
            }

            let utc: DateTime<Utc> = Utc::now();

            let volumes = match serde_json::to_string(&input.volumes) {
                Ok(json_str) => json_str,
                Err(e) => {
                    log::error!("Volume serialization error: {}", e);
                    let message = Message {
                        message: "Internal server error".to_string(),
                    };
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
                status: DeploymentStatus::Creating,
                created_at: utc.to_string(),
                updated_at: None,
                labels: input.labels,
                environment: input.environment,
                replicas: input.replicas,
                command: input.command,
                instances: [].to_vec(),
                restart_count: 0,
                volumes,
                health_checks: input.health_checks.unwrap_or_default(),
                resources: input.resources,
                image_digest: None,
                ports: input.ports,
                pending_events: vec![],
                parent_id: rolling_parent_id,
                network: input.network.clone(),
            };

            match deployments::create(&pool, &deployment).await {
                Ok(deployment) => {
                    let _ = deployment_event::log_event(
                        &pool,
                        deployment.id.clone(),
                        "info",
                        format!("Deployment '{}' created successfully", deployment.name),
                        "api",
                        Some("deployment_created"),
                    )
                    .await;

                    // When a previous active deployment was wiped instead of
                    // being kept as a rolling parent, surface the reason as a
                    // dedicated event. Operators inspecting "why didn't my
                    // rolling update happen?" find the answer in one place
                    // (event level: warning so it shows up under
                    // `--level warning` filters).
                    if let Some(reason) = replace_reason {
                        let replaced = if replaced_deployment_ids.len() == 1 {
                            format!("deployment {}", replaced_deployment_ids[0])
                        } else {
                            format!(
                                "{} deployments ({})",
                                replaced_deployment_ids.len(),
                                replaced_deployment_ids.join(", ")
                            )
                        };
                        let message = match reason {
                            "force" => format!(
                                "Replaced {} immediately because force=true was set on the request — rolling update skipped",
                                replaced
                            ),
                            "no_health_checks" => format!(
                                "Replaced {} immediately because no health checks are declared — rolling update requires at least one health check",
                                replaced
                            ),
                            "multiple_active_deployments" => format!(
                                "Replaced {} immediately because more than one active deployment was found for {}/{} — rolling update only applies when exactly one parent exists",
                                replaced, deployment.namespace, deployment.name
                            ),
                            other => format!("Replaced {} immediately ({})", replaced, other),
                        };
                        let _ = deployment_event::log_event(
                            &pool,
                            deployment.id.clone(),
                            "warning",
                            message,
                            "api",
                            Some("force_replace"),
                        )
                        .await;
                    }

                    let deployment_output = DeploymentOutput::from_to_model(deployment);
                    (StatusCode::CREATED, Json(deployment_output)).into_response()
                }
                Err(e) => {
                    error!("Failed to create deployment: {}", e);
                    let message = Message {
                        message: format!(
                            "A deployment with name '{}' already exists in namespace '{}'",
                            input.name, input.namespace
                        ),
                    };
                    (StatusCode::CONFLICT, Json(message)).into_response()
                }
            }
        }
        Err(e) => {
            let message = Message {
                message: e.to_string(),
            };
            (StatusCode::BAD_REQUEST, Json(message)).into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::server::tests::{login, new_test_app, new_test_app_with_pool};
    use axum_test::{TestResponse, TestServer};
    use serde_json::json;

    #[tokio::test]
    async fn create_with_invalid_runtime() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post("/deployments")
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
    async fn create_cloud_hypervisor_accepts_volumes() {
        // virtio-fs covers all three volume types on the CH runtime, so the
        // API no longer rejects them. The runtime layer is responsible for
        // spawning virtiofsd and enriching cloud-init at boot.
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post("/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "runtime": "cloud-hypervisor",
                "name": "vm-with-vol",
                "namespace": "ring",
                "image": "/tmp/fake.raw",
                "volumes": [
                    {
                        "type": "bind",
                        "source": "/host",
                        "destination": "/guest",
                        "driver": "local",
                        "permission": "rw"
                    }
                ]
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn create_cloud_hypervisor_accepts_command_health_check() {
        // command health checks now route through `ring-agent` over vsock.
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post("/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "runtime": "cloud-hypervisor",
                "name": "vm-with-cmd-hc",
                "namespace": "ring",
                "image": "/tmp/fake.raw",
                "health_checks": [
                    {
                        "type": "command",
                        "command": "/bin/true",
                        "interval": "10s",
                        "timeout": "2s",
                        "on_failure": "restart"
                    }
                ]
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn create_cloud_hypervisor_accepts_environment() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post("/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "runtime": "cloud-hypervisor",
                "name": "vm-with-env",
                "namespace": "ring",
                "image": "/tmp/fake.raw",
                "environment": { "FOO": "bar" }
            }))
            .await;

        // env vars are now injected via cloud-init NoCloud.
        assert_eq!(response.status_code(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn create_cloud_hypervisor_rejects_command() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post("/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "runtime": "cloud-hypervisor",
                "name": "vm-with-cmd",
                "namespace": "ring",
                "image": "/tmp/fake.raw",
                "command": ["/bin/sh", "-c", "echo hi"]
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::BAD_REQUEST);
        let body: Message = response.json();
        assert!(
            body.message.contains("custom commands are not supported"),
            "unexpected error: {}",
            body.message
        );
    }

    #[tokio::test]
    async fn create_cloud_hypervisor_rejects_docker_image_reference() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post("/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "runtime": "cloud-hypervisor",
                "name": "vm-with-docker-image",
                "namespace": "ring",
                "image": "nginx:latest"
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::BAD_REQUEST);
        let body: Message = response.json();
        assert!(
            body.message.contains("absolute path to a raw disk image"),
            "unexpected error: {}",
            body.message
        );
    }

    #[tokio::test]
    async fn create_cloud_hypervisor_accepts_tcp_health_check() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post("/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "runtime": "cloud-hypervisor",
                "name": "vm-with-tcp-hc",
                "namespace": "ring",
                "image": "/tmp/fake.raw",
                "health_checks": [
                    {
                        "type": "tcp",
                        "port": 80,
                        "interval": "10s",
                        "timeout": "2s",
                        "on_failure": "restart"
                    }
                ]
            }))
            .await;

        // Accepted at validation. Runtime-level failures (missing image, etc.)
        // happen later in the scheduler, not in the API handler.
        assert_eq!(response.status_code(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn create_with_without_auth() {
        let app = new_test_app().await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post("/deployments")
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
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post("/deployments")
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
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post("/deployments")
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
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post("/deployments")
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
                        "permission": "invalid_permission"
                    }
                ]
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::UNPROCESSABLE_ENTITY);

        let error_text = response.text();
        assert!(
            error_text.contains("unknown variant") || error_text.contains("invalid_permission")
        );
    }

    #[tokio::test]
    async fn create_with_bind_volume_missing_source() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post("/deployments")
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
                    }
                ]
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::BAD_REQUEST);
        let error_body: Message = response.json();
        assert!(
            error_body
                .message
                .contains("source is required for bind volumes")
        );
    }

    #[tokio::test]
    async fn create_with_invalid_volume_driver() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post("/deployments")
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
                        "driver": "invalid_driver",
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
    async fn create_with_bind_volume_empty_source() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post("/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "runtime": "docker",
                "name": "nginx",
                "namespace": "ring",
                "image": "nginx:latest",
                "volumes": [
                    {
                        "type": "bind",
                        "source": "",
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
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post("/deployments")
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
                    }
                ]
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::BAD_REQUEST);
        let error_body: Message = response.json();
        assert!(
            error_body
                .message
                .contains("source is required for config volumes")
                || error_body
                    .message
                    .contains("key is required for config volumes")
        );
    }

    #[tokio::test]
    async fn create_with_config_volume_empty_config_reference() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post("/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "runtime": "docker",
                "name": "nginx",
                "namespace": "ring",
                "image": "nginx:latest",
                "volumes": [
                    {
                        "type": "config",
                        "source": "",
                        "key": "",
                        "destination": "/etc/nginx/nginx.conf",
                        "driver": "local",
                        "permission": "ro"
                    }
                ]
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::BAD_REQUEST);
        let error_body: Message = response.json();
        assert!(
            error_body.message.contains("source cannot be empty")
                || error_body.message.contains("key cannot be empty")
        );
    }

    #[tokio::test]
    async fn create_with_volume_empty_destination() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post("/deployments")
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
                        "destination": "",
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
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post("/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "runtime": "docker",
                "name": "nginx",
                "namespace": "ring",
                "image": "nginx:latest",
                "volumes": [
                    {
                        "type": "invalid_type",
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
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post("/deployments")
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
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post("/deployments")
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
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post("/deployments")
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
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post("/deployments")
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
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post("/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "runtime": "docker",
                "name": "nginx",
                "namespace": "ring",
                "image": "nginx:latest",
                "volumes": [
                    {
                        "type": "bind",
                        "destination": "",
                        "driver": "local",
                        "permission": "ro"
                    }
                ]
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::BAD_REQUEST);
        let error_body: Message = response.json();
        let message = &error_body.message;
        assert!(message.contains("source") || message.contains("destination"));
    }

    #[tokio::test]
    async fn create_with_config_volume_missing_source_only() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post("/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "runtime": "docker",
                "name": "nginx",
                "namespace": "ring",
                "image": "nginx:latest",
                "volumes": [
                    {
                        "type": "config",
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
        assert!(
            error_body
                .message
                .contains("source is required for config volumes")
        );
    }

    #[tokio::test]
    async fn create_with_config_volume_missing_key_only() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post("/deployments")
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
                        "destination": "/etc/nginx/nginx.conf",
                        "driver": "local",
                        "permission": "ro"
                    }
                ]
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::BAD_REQUEST);
        let error_body: Message = response.json();
        assert!(
            error_body
                .message
                .contains("key is required for config volumes")
        );
    }

    #[tokio::test]
    async fn create_with_config_volume_empty_source_only() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post("/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "runtime": "docker",
                "name": "nginx",
                "namespace": "ring",
                "image": "nginx:latest",
                "volumes": [
                    {
                        "type": "config",
                        "source": "",
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
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post("/deployments")
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
                        "key": "",
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
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post("/deployments")
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
                    }
                ]
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::BAD_REQUEST);
        let error_body: Message = response.json();
        assert!(
            error_body
                .message
                .contains("source is required for named volumes")
        );
    }

    #[tokio::test]
    async fn create_with_named_volume_empty_source() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post("/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "runtime": "docker",
                "name": "nginx",
                "namespace": "ring",
                "image": "nginx:latest",
                "volumes": [
                    {
                        "type": "volume",
                        "source": "",
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
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post("/deployments")
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
                        "permission": "rw"
                    }
                ]
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::BAD_REQUEST);
        let error_body: Message = response.json();
        assert!(
            error_body
                .message
                .contains("config volumes must be read-only")
        );
    }

    #[tokio::test]
    async fn create_worker_with_json_array_command() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post("/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "kind": "worker",
                "runtime": "docker",
                "name": "echo-worker",
                "namespace": "test",
                "image": "alpine:latest",
                "command": ["sh", "-c", "while true; do echo 'Worker running'; sleep 30; done"],
                "replicas": 2
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::CREATED);

        let deployment: DeploymentOutput = response.json();
        assert_eq!(deployment.kind, "worker");
        assert_eq!(deployment.replicas, 2);
    }

    #[tokio::test]
    async fn create_with_health_checks() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post("/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "runtime": "docker",
                "name": "web-service",
                "namespace": "production",
                "image": "nginx:latest",
                "health_checks": [
                    {
                        "type": "tcp",
                        "port": 8080,
                        "interval": "10s",
                        "timeout": "5s",
                        "threshold": 3,
                        "on_failure": "restart"
                    },
                    {
                        "type": "http",
                        "url": "http://localhost:8080/health",
                        "interval": "30s",
                        "timeout": "10s",
                        "threshold": 2,
                        "on_failure": "alert"
                    },
                    {
                        "type": "command",
                        "command": "curl -f http://localhost:8080/ping",
                        "interval": "20s",
                        "timeout": "5s",
                        "threshold": 1,
                        "on_failure": "stop"
                    }
                ]
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::CREATED);

        let deployment: DeploymentOutput = response.json();
        assert_eq!(deployment.name, "web-service");
        assert_eq!(deployment.namespace, "production");
        assert_eq!(deployment.health_checks.len(), 3);

        let check_types: Vec<String> = deployment
            .health_checks
            .iter()
            .map(|check| check.check_type().to_string())
            .collect();
        assert!(check_types.contains(&"tcp".to_string()));
        assert!(check_types.contains(&"http".to_string()));
        assert!(check_types.contains(&"command".to_string()));
    }

    #[tokio::test]
    async fn create_with_resources() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post("/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "runtime": "docker",
                "name": "limited-nginx",
                "namespace": "ring",
                "image": "nginx:latest",
                "resources": {
                    "limits": {
                        "cpu": "0.5",
                        "memory": "512Mi"
                    },
                    "requests": {
                        "cpu": "0.25",
                        "memory": "256Mi"
                    }
                }
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::CREATED);

        let deployment: DeploymentOutput = response.json();
        assert_eq!(deployment.name, "limited-nginx");
        let resources = deployment.resources.expect("resources should be present");
        let limits = resources.limits.expect("limits should be present");
        assert_eq!(limits.cpu, Some("0.5".to_string()));
        assert_eq!(limits.memory, Some("512Mi".to_string()));
        let requests = resources.requests.expect("requests should be present");
        assert_eq!(requests.cpu, Some("0.25".to_string()));
        assert_eq!(requests.memory, Some("256Mi".to_string()));
    }

    #[tokio::test]
    async fn create_without_resources() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post("/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "runtime": "docker",
                "name": "no-limits-nginx",
                "namespace": "ring",
                "image": "nginx:latest"
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::CREATED);

        let deployment: DeploymentOutput = response.json();
        assert!(deployment.resources.is_none());
    }

    #[tokio::test]
    async fn create_with_partial_resources() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post("/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "runtime": "docker",
                "name": "partial-nginx",
                "namespace": "ring",
                "image": "nginx:latest",
                "resources": {
                    "limits": {
                        "memory": "1Gi"
                    }
                }
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::CREATED);

        let deployment: DeploymentOutput = response.json();
        let resources = deployment.resources.expect("resources should be present");
        let limits = resources.limits.expect("limits should be present");
        assert_eq!(limits.memory, Some("1Gi".to_string()));
        assert!(limits.cpu.is_none());
        assert!(resources.requests.is_none());
    }

    #[tokio::test]
    async fn create_returns_null_image_digest() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post("/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "runtime": "docker",
                "name": "digest-test",
                "namespace": "ring",
                "image": "nginx:latest"
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::CREATED);
        let body: serde_json::Value = response.json();
        assert!(body.get("image_digest").is_none() || body["image_digest"].is_null());
    }

    #[tokio::test]
    async fn create_with_invalid_health_check_threshold() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post("/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "runtime": "docker",
                "name": "test-service",
                "namespace": "test",
                "image": "nginx:latest",
                "health_checks": [
                    {
                        "type": "tcp",
                        "port": 8080,
                        "interval": "10s",
                        "timeout": "5s",
                        "threshold": -1,  // Invalid negative threshold
                        "on_failure": "restart"
                    }
                ]
            }))
            .await;

        assert!(
            response.status_code() == StatusCode::CREATED
                || response.status_code() == StatusCode::BAD_REQUEST
                || response.status_code() == StatusCode::UNPROCESSABLE_ENTITY
        );
    }

    #[tokio::test]
    async fn create_auto_creates_namespace() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        // Verify namespace doesn't exist yet
        let response = server
            .get("/namespaces")
            .add_header("Authorization", format!("Bearer {}", token))
            .await;
        let namespaces: Vec<crate::api::dto::namespace::NamespaceOutput> = response.json();
        assert!(namespaces.is_empty());

        // Create a deployment in a new namespace
        let response = server
            .post("/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "runtime": "docker",
                "name": "nginx",
                "namespace": "auto-created-ns",
                "image": "nginx:latest"
            }))
            .await;
        assert_eq!(response.status_code(), StatusCode::CREATED);

        // Verify namespace was auto-created
        let response = server
            .get("/namespaces")
            .add_header("Authorization", format!("Bearer {}", token))
            .await;
        let namespaces: Vec<crate::api::dto::namespace::NamespaceOutput> = response.json();
        assert_eq!(namespaces.len(), 1);
        assert_eq!(namespaces[0].name, "auto-created-ns");
    }

    #[tokio::test]
    async fn rolling_update_sets_parent_id_with_health_checks() {
        let (pool, app) = new_test_app_with_pool().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        // Create initial deployment with health checks
        let response = server
            .post("/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "runtime": "docker",
                "name": "rolling-app",
                "namespace": "rolling-ns",
                "image": "nginx:1.0",
                "health_checks": [{"type": "tcp", "port": 80, "interval": "10s", "timeout": "5s", "on_failure": "restart"}]
            }))
            .await;
        assert_eq!(response.status_code(), StatusCode::CREATED);
        let first: serde_json::Value = response.json();
        let first_id = first["id"].as_str().unwrap().to_string();

        // Manually set status to running so it qualifies as active
        sqlx::query("UPDATE deployment SET status = 'running' WHERE id = ?")
            .bind(&first_id)
            .execute(&pool)
            .await
            .unwrap();

        // Re-apply with new image and health checks → should trigger rolling update
        let response = server
            .post("/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "runtime": "docker",
                "name": "rolling-app",
                "namespace": "rolling-ns",
                "image": "nginx:2.0",
                "health_checks": [{"type": "tcp", "port": 80, "interval": "10s", "timeout": "5s", "on_failure": "restart"}]
            }))
            .await;
        assert_eq!(response.status_code(), StatusCode::CREATED);

        // Check parent deployment is still running (not deleted)
        let response = server
            .get(&format!("/deployments/{}", first_id))
            .add_header("Authorization", format!("Bearer {}", token))
            .await;
        assert_eq!(response.status_code(), StatusCode::OK);
        let parent: serde_json::Value = response.json();
        assert_eq!(parent["status"], "running");
    }

    #[tokio::test]
    async fn force_flag_bypasses_rolling_update() {
        let (pool, app) = new_test_app_with_pool().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        // Create initial deployment with health checks
        let response = server
            .post("/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "runtime": "docker",
                "name": "force-app",
                "namespace": "force-ns",
                "image": "nginx:1.0",
                "health_checks": [{"type": "tcp", "port": 80, "interval": "10s", "timeout": "5s", "on_failure": "restart"}]
            }))
            .await;
        assert_eq!(response.status_code(), StatusCode::CREATED);
        let first: serde_json::Value = response.json();
        let first_id = first["id"].as_str().unwrap().to_string();

        // Set status to running
        sqlx::query("UPDATE deployment SET status = 'running' WHERE id = ?")
            .bind(&first_id)
            .execute(&pool)
            .await
            .unwrap();

        // Re-apply with --force → should NOT do rolling update
        let response = server
            .post("/deployments?force=true")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "runtime": "docker",
                "name": "force-app",
                "namespace": "force-ns",
                "image": "nginx:2.0",
                "health_checks": [{"type": "tcp", "port": 80, "interval": "10s", "timeout": "5s", "on_failure": "restart"}]
            }))
            .await;
        assert_eq!(response.status_code(), StatusCode::CREATED);

        // Check parent deployment was marked as deleted
        let response = server
            .get(&format!("/deployments/{}", first_id))
            .add_header("Authorization", format!("Bearer {}", token))
            .await;
        assert_eq!(response.status_code(), StatusCode::OK);
        let parent: serde_json::Value = response.json();
        assert_eq!(parent["status"], "deleted");

        // The new deployment must carry a ForceReplace event explaining why
        // the parent was wiped instead of kept as a rolling parent.
        let list_response = server
            .get("/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .await;
        let deployments: Vec<serde_json::Value> = list_response.json();
        let new_deployment = deployments
            .iter()
            .find(|d| d["name"] == "force-app" && d["id"].as_str() != Some(&first_id))
            .expect("new deployment must exist");
        let new_id = new_deployment["id"].as_str().unwrap();
        let event: (String, String) = sqlx::query_as(
            "SELECT level, message FROM deployment_event WHERE deployment_id = ? AND reason = 'force_replace'",
        )
        .bind(new_id)
        .fetch_one(&pool)
        .await
        .expect("ForceReplace event must be logged");
        assert_eq!(event.0, "warning");
        assert!(event.1.contains("force=true"), "got message: {}", event.1);
    }

    #[tokio::test]
    async fn no_health_checks_bypasses_rolling_update() {
        let (pool, app) = new_test_app_with_pool().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        // Create initial deployment WITHOUT health checks
        let response = server
            .post("/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "runtime": "docker",
                "name": "nohc-app",
                "namespace": "nohc-ns",
                "image": "nginx:1.0"
            }))
            .await;
        assert_eq!(response.status_code(), StatusCode::CREATED);
        let first: serde_json::Value = response.json();
        let first_id = first["id"].as_str().unwrap().to_string();

        // Set status to running
        sqlx::query("UPDATE deployment SET status = 'running' WHERE id = ?")
            .bind(&first_id)
            .execute(&pool)
            .await
            .unwrap();

        // Re-apply without health checks → should NOT do rolling update
        let response = server
            .post("/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "runtime": "docker",
                "name": "nohc-app",
                "namespace": "nohc-ns",
                "image": "nginx:2.0"
            }))
            .await;
        assert_eq!(response.status_code(), StatusCode::CREATED);

        // Check parent deployment was marked as deleted
        let response = server
            .get(&format!("/deployments/{}", first_id))
            .add_header("Authorization", format!("Bearer {}", token))
            .await;
        assert_eq!(response.status_code(), StatusCode::OK);
        let parent: serde_json::Value = response.json();
        assert_eq!(parent["status"], "deleted");

        // The new deployment must carry a ForceReplace event with reason
        // "no_health_checks" so the operator can fix the config.
        let list_response = server
            .get("/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .await;
        let deployments: Vec<serde_json::Value> = list_response.json();
        let new_deployment = deployments
            .iter()
            .find(|d| d["name"] == "nohc-app" && d["id"].as_str() != Some(&first_id))
            .expect("new deployment must exist");
        let new_id = new_deployment["id"].as_str().unwrap();
        let event: (String, String) = sqlx::query_as(
            "SELECT level, message FROM deployment_event WHERE deployment_id = ? AND reason = 'force_replace'",
        )
        .bind(new_id)
        .fetch_one(&pool)
        .await
        .expect("ForceReplace event must be logged");
        assert_eq!(event.0, "warning");
        assert!(
            event.1.contains("no health checks"),
            "got message: {}",
            event.1
        );
    }

    #[tokio::test]
    async fn create_with_ports() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post("/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "runtime": "docker",
                "name": "nginx",
                "namespace": "ring",
                "image": "nginx:latest",
                "ports": [
                    { "published": 8080, "target": 80 },
                    { "published": 3000, "target": 3000 }
                ]
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::CREATED);
        let body: serde_json::Value = response.json();
        let ports = body["ports"].as_array().unwrap();
        assert_eq!(ports.len(), 2);
        assert_eq!(ports[0]["published"], 8080);
        assert_eq!(ports[0]["target"], 80);
        assert_eq!(ports[1]["published"], 3000);
        assert_eq!(ports[1]["target"], 3000);
    }

    #[tokio::test]
    async fn create_with_host_network_mode() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post("/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "runtime": "docker",
                "name": "haproxy",
                "namespace": "edge",
                "image": "haproxy:2.9",
                "network": { "mode": "host" }
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::CREATED);
        let body: serde_json::Value = response.json();
        assert_eq!(body["network"]["mode"], "host");
    }

    #[tokio::test]
    async fn create_host_mode_rejects_ports() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post("/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "runtime": "docker",
                "name": "haproxy",
                "namespace": "edge",
                "image": "haproxy:2.9",
                "network": { "mode": "host" },
                "ports": [{ "published": 80, "target": 80 }]
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::BAD_REQUEST);
        let body: Message = response.json();
        assert!(
            body.message.contains("network.mode=host") && body.message.contains("port"),
            "unexpected message: {}",
            body.message
        );
    }

    #[tokio::test]
    async fn create_host_mode_rejects_replicas_above_one() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post("/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "runtime": "docker",
                "name": "haproxy",
                "namespace": "edge",
                "image": "haproxy:2.9",
                "network": { "mode": "host" },
                "replicas": 2
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::BAD_REQUEST);
        let body: Message = response.json();
        assert!(
            body.message.contains("replicas"),
            "unexpected message: {}",
            body.message
        );
    }

    #[tokio::test]
    async fn create_host_mode_rejected_on_cloud_hypervisor() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post("/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "runtime": "cloud-hypervisor",
                "name": "vm",
                "namespace": "edge",
                "image": "/tmp/fake.raw",
                "network": { "mode": "host" }
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::BAD_REQUEST);
        let body: Message = response.json();
        assert!(
            body.message.contains("docker runtime"),
            "unexpected message: {}",
            body.message
        );
    }

    #[tokio::test]
    async fn create_with_bridge_network_mode_explicit() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post("/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "runtime": "docker",
                "name": "nginx",
                "namespace": "ring",
                "image": "nginx:latest",
                "network": { "mode": "bridge" },
                "ports": [{ "published": 8080, "target": 80 }],
                "replicas": 2
            }))
            .await;

        // bridge mode is the existing default — ports and replicas remain valid.
        assert_eq!(response.status_code(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn create_without_ports_defaults_to_empty() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post("/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "runtime": "docker",
                "name": "nginx",
                "namespace": "ring",
                "image": "nginx:latest"
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::CREATED);
        let body: serde_json::Value = response.json();
        let ports = body["ports"].as_array().unwrap();
        assert!(ports.is_empty());
    }
}
