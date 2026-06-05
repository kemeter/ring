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

use crate::api::auth::{Auth, require_namespace};
use crate::api::dto::deployment::DeploymentOutput;
use crate::api::server::Db;
use crate::api::validation::{Violation, ViolationList};
use crate::models::audit_log;
use crate::models::deployment_event;
use crate::models::deployments;
use crate::models::deployments::{
    Deployment, DeploymentConfig, DeploymentPort, DeploymentStatus, EnvValue, NetworkConfig,
    NetworkMode, Resource, default_image_pull_policy,
};
use crate::models::namespace;

fn default_replicas() -> u32 {
    1
}

fn validate_runtime(runtime: &str) -> Result<(), ValidationError> {
    match runtime {
        "docker" | "podman" | "cloud-hypervisor" => Ok(()),
        _ => Err(
            ValidationError::new("deployment.runtime.unsupported").with_message(Cow::Borrowed(
                "runtime must be one of: docker, podman, cloud-hypervisor",
            )),
        ),
    }
}

fn validate_network_constraints(input: &DeploymentInput, errors: &mut ViolationList) {
    let Some(network) = &input.network else {
        return;
    };
    if !matches!(network.mode, NetworkMode::Host) {
        return;
    }

    if input.runtime != "docker" {
        errors.push(Violation::new(
            "network.mode",
            format!(
                "host networking is only supported on the docker runtime, got '{}'",
                input.runtime
            ),
            "deployment.network.host_runtime_unsupported",
        ));
    }

    if !input.ports.is_empty() {
        errors.push(Violation::new(
            "ports",
            "host networking bypasses Docker's port bindings; remove `ports` and let the container bind directly on the host",
            "deployment.ports.host_network_conflict",
        ));
    }

    if input.replicas > 1 {
        errors.push(Violation::new(
            "replicas",
            format!(
                "host networking is incompatible with replicas > 1 (got {}): all replicas would compete for the same host ports",
                input.replicas
            ),
            "deployment.replicas.host_network_conflict",
        ));
    }
}

/// Validate environment variable names against POSIX/Docker rules:
/// `[A-Za-z_][A-Za-z0-9_]*`. Names like `bad-name` or `1NOT_ALLOWED`
/// silently become unusable variables in the container shell — surface
/// them at API time so the user catches typos before deploy.
fn validate_environment(input: &DeploymentInput, errors: &mut ViolationList) {
    static ENV_VAR_PATTERN: once_cell::sync::Lazy<regex::Regex> =
        once_cell::sync::Lazy::new(|| regex::Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*$").unwrap());

    for key in input.environment.keys() {
        if !ENV_VAR_PATTERN.is_match(key) {
            errors.push(Violation::new(
                format!("environment.{}", key),
                format!(
                    "'{}' is not a valid env var name (must match [A-Za-z_][A-Za-z0-9_]*)",
                    key
                ),
                "deployment.environment.key.invalid",
            ));
        }
    }
}

/// Validate `resources.limits.{cpu,memory}` and `resources.requests.{cpu,memory}`
/// strings parse correctly. `parse_cpu_string` accepts forms like `"500m"` or
/// `"2"`; `parse_memory_string` handles binary (`Ki`, `Mi`, …) and decimal
/// (`K`, `M`, …) suffixes. Anything else used to be a silent runtime crash.
fn validate_resources(input: &DeploymentInput, errors: &mut ViolationList) {
    let Some(resources) = &input.resources else {
        return;
    };
    let check = |spec: Option<&crate::models::deployments::ResourceSpec>,
                 kind: &str,
                 errors: &mut ViolationList| {
        let Some(spec) = spec else { return };
        if let Some(cpu) = &spec.cpu
            && crate::models::deployments::parse_cpu_string(cpu).is_err()
        {
            errors.push(Violation::new(
                format!("resources.{}.cpu", kind),
                format!("'{}' is not a valid CPU spec (e.g. '500m' or '2')", cpu),
                format!("deployment.resources.{}.cpu.invalid", kind),
            ));
        }
        if let Some(memory) = &spec.memory
            && crate::models::deployments::parse_memory_string(memory).is_err()
        {
            errors.push(Violation::new(
                format!("resources.{}.memory", kind),
                format!(
                    "'{}' is not a valid memory spec (e.g. '512Mi' or '2Gi')",
                    memory
                ),
                format!("deployment.resources.{}.memory.invalid", kind),
            ));
        }
    };
    check(resources.limits.as_ref(), "limits", errors);
    check(resources.requests.as_ref(), "requests", errors);
}

/// Validate `config.image_pull_policy` is one of Docker's accepted values.
/// Anything else is silently ignored at runtime; reject upfront.
fn validate_config(input: &DeploymentInput, errors: &mut ViolationList) {
    let Some(config) = &input.config else {
        return;
    };
    let policy = config.image_pull_policy.as_str();
    if !matches!(policy, "Always" | "IfNotPresent" | "Never") {
        errors.push(Violation::new(
            "config.image_pull_policy",
            format!(
                "'{}' is not a supported pull policy (must be one of: Always, IfNotPresent, Never)",
                policy
            ),
            "deployment.config.image_pull_policy.unsupported",
        ));
    }
}

/// Per-port rules. The `DeploymentPort` struct already constrains
/// `published`/`target` to `u16` so anything > 65535 is caught at deserialize
/// time; we still need to reject `0` (reserved + "any port" semantics) and
/// to surface duplicate `published` values that would race for the same host
/// port. Paths use JSONPath form (`ports[idx].published`) so a client can
/// point straight at the offending entry.
fn validate_ports(input: &DeploymentInput, errors: &mut ViolationList) {
    use std::collections::HashMap;

    // Keyed by (published port, protocol): the same host port may be bound
    // once for TCP and once for UDP — those are separate namespaces — but not
    // twice for the same protocol.
    let mut published_seen: HashMap<(u16, crate::models::deployments::PortProtocol), usize> =
        HashMap::new();

    for (idx, port) in input.ports.iter().enumerate() {
        if port.published == 0 {
            errors.push(Violation::new(
                format!("ports[{}].published", idx),
                "must be between 1 and 65535",
                "deployment.ports.published.out_of_range",
            ));
        }
        if port.target == 0 {
            errors.push(Violation::new(
                format!("ports[{}].target", idx),
                "must be between 1 and 65535",
                "deployment.ports.target.out_of_range",
            ));
        }

        // `host_ip` reaches Docker's PortBindings / socat's bind= verbatim.
        // A malformed value would otherwise surface only at runtime as an
        // opaque error event, so reject it here with a field-scoped path.
        if let Some(host_ip) = &port.host_ip
            && host_ip.parse::<std::net::IpAddr>().is_err()
        {
            errors.push(Violation::new(
                format!("ports[{}].host_ip", idx),
                format!("'{}' is not a valid IP address", host_ip),
                "deployment.ports.host_ip.invalid",
            ));
        }

        if port.published != 0 {
            let key = (port.published, port.protocol);
            if let Some(prev_idx) = published_seen.get(&key) {
                errors.push(Violation::new(
                    format!("ports[{}].published", idx),
                    format!(
                        "duplicates ports[{}].published = {}/{}; each host port can only be bound once per protocol",
                        prev_idx,
                        port.published,
                        port.protocol.as_str()
                    ),
                    "deployment.ports.published.duplicate",
                ));
            } else {
                published_seen.insert(key, idx);
            }
        }
    }
}

/// Cross-field rules that catch configurations which are syntactically valid
/// but semantically broken. Every rule pushes one violation per affected
/// field — when a rule could be fixed by changing either of two fields, both
/// get a violation so the user picks which one to change.
fn validate_cross_field_constraints(input: &DeploymentInput, errors: &mut ViolationList) {
    // `ports[] + replicas > 1`: every replica would race for the same
    // host port. Either drop the ports or scale down to 1.
    if !input.ports.is_empty() && input.replicas > 1 {
        errors.push(Violation::new(
            "ports",
            format!(
                "publishing host ports with replicas > 1 ({}) causes port conflicts between replicas — drop `ports` or reduce `replicas` to 1",
                input.replicas
            ),
            "deployment.ports.replicas_conflict",
        ));
        errors.push(Violation::new(
            "replicas",
            format!(
                "replicas > 1 ({}) is incompatible with `ports` — drop `ports` or reduce `replicas` to 1",
                input.replicas
            ),
            "deployment.replicas.ports_conflict",
        ));
    }

    // `volume (rw) + replicas > 1`: every replica would mount the same named
    // volume read-write. The local driver gives no cross-container coordination,
    // so concurrent writers (e.g. a database) corrupt each other's data silently.
    // Read-only sharing is safe. Either scale down to 1 or mount the volume `ro`.
    if input.replicas > 1 {
        for (idx, volume) in input.volumes.iter().enumerate() {
            if matches!(volume.r#type, VolumeType::Volume)
                && matches!(volume.permission, Permission::Rw)
            {
                let source = volume.source.as_deref().unwrap_or("<unnamed>");
                errors.push(Violation::new(
                    format!("volumes[{}].permission", idx),
                    format!(
                        "named volume '{}' is mounted read-write with replicas > 1 ({}); replicas share the same volume with no write coordination, which corrupts data — reduce `replicas` to 1 or mount the volume `ro`",
                        source, input.replicas
                    ),
                    "deployment.volumes.shared_rw_replicas",
                ));
            }
        }
    }

    // `kind: job + replicas > 1`: a job is one-shot. Multiple replicas
    // would mean N parallel runs of the same task which is not what the
    // job kind models.
    if matches!(input.kind, DeploymentKind::Job) && input.replicas > 1 {
        errors.push(Violation::new(
            "replicas",
            format!(
                "kind=job runs once and exits; replicas must be 1, got {}",
                input.replicas
            ),
            "deployment.replicas.job_must_be_one",
        ));
    }

    // `kind: job + readiness check`: readiness gates a rolling update.
    // Jobs don't roll — they run once. A readiness flag here is a config
    // gap that would never trigger anything useful.
    if matches!(input.kind, DeploymentKind::Job)
        && let Some(hcs) = input.health_checks.as_ref()
        && hcs.iter().any(|hc| hc.is_readiness())
    {
        errors.push(Violation::new(
            "health_checks",
            "kind=job is incompatible with readiness health checks (readiness gates rolling updates, which don't apply to one-shot jobs)",
            "deployment.health_checks.job_readiness_unsupported",
        ));
    }
}

fn validate_runtime_constraints(input: &DeploymentInput, errors: &mut ViolationList) {
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
            errors.push(Violation::new(
                "command",
                "custom commands are not supported on cloud-hypervisor runtime (alpha); the VM boots whatever its image is configured to run",
                "deployment.command.cloud_hypervisor_unsupported",
            ));
        }

        // CH expects a raw disk image path on the host, not a Docker image
        // reference. Anything that doesn't start with '/' is almost certainly
        // a Docker reference (e.g. `nginx:latest`) — fail early instead of
        // letting it fail much later with a confusing "VM image not found".
        if !input.image.starts_with('/') {
            errors.push(Violation::new(
                "image",
                format!(
                    "cloud-hypervisor runtime expects an absolute path to a raw disk image, got '{}' (Docker image references are not supported)",
                    input.image
                ),
                "deployment.image.cloud_hypervisor_requires_absolute_path",
            ));
        }
    }
}

/// Fields that are accepted by the API but have no effect on the
/// cloud-hypervisor runtime, returned by their manifest path so the warning
/// names exactly what the user wrote. CH sizes the VM from `resources.limits`
/// only and never pulls from a registry, so `resources.requests` and the
/// `config.*` registry/user knobs are dropped.
fn cloud_hypervisor_ignored_fields(deployment: &Deployment) -> Vec<String> {
    let mut ignored = Vec::new();

    if deployment
        .resources
        .as_ref()
        .is_some_and(|r| r.requests.is_some())
    {
        ignored.push("resources.requests".to_string());
    }

    if let Some(config) = &deployment.config {
        // `image_pull_policy` has a default, so only flag it when it isn't the
        // default — a user who explicitly set `Never`/`IfNotPresent` should
        // know it does nothing here.
        if config.image_pull_policy != default_image_pull_policy() {
            ignored.push("config.image_pull_policy".to_string());
        }
        if config.server.is_some() {
            ignored.push("config.server".to_string());
        }
        if config.username.is_some() {
            ignored.push("config.username".to_string());
        }
        if config.password.is_some() {
            ignored.push("config.password".to_string());
        }
        if config.user.is_some() {
            ignored.push("config.user".to_string());
        }
    }

    ignored
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum VolumeType {
    #[default]
    Bind,
    Config,
    Volume,
    Secret,
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
            VolumeType::Secret => {
                match &self.source {
                    None => {
                        errors.add(
                            "source",
                            ValidationError::new("source is required for secret volumes"),
                        );
                    }
                    Some(source) if source.is_empty() => {
                        errors.add("source", ValidationError::new("source cannot be empty"));
                    }
                    _ => {}
                }

                // A secret holds a single opaque value — there is no `key:`
                // to pick. Reject explicitly so users coming from Kubernetes
                // (where Secret resources are multi-key dicts) get a clear
                // error instead of silent omission.
                if let Some(key) = &self.key
                    && !key.is_empty()
                {
                    let error = ValidationError {
                        code: Cow::from("unexpected_field"),
                        message: Some(Cow::from(
                            "secret volumes have no key — a secret holds a single opaque value",
                        )),
                        params: HashMap::new(),
                    };
                    errors.add("key", error);
                }

                if !matches!(self.permission, Permission::Ro) {
                    let error = ValidationError {
                        code: Cow::from("invalid_permission"),
                        message: Some(Cow::from("secret volumes must be read-only (ro)")),
                        params: HashMap::new(),
                    };
                    errors.add("permission", error);
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
    auth: Auth,
    Query(params): Query<CreateQueryParams>,
    Json(input): Json<DeploymentInput>,
) -> impl IntoResponse {
    // Accumulate every validation error in one pass: a manifest that
    // violates several rules surfaces the full list in one response so
    // the user can fix everything in one apply cycle. Order:
    //   1. `validator` field rules driven by #[validate(...)] attributes
    //   2. Runtime-specific constraints (e.g. cloud-hypervisor expects an
    //      absolute image path, no custom command, …)
    //   3. Cross-field constraints (e.g. host networking + replicas > 1).
    let mut violations = ViolationList::new();
    if let Err(errs) = input.validate() {
        violations.extend_from_validator(errs);
    }
    validate_runtime_constraints(&input, &mut violations);
    validate_network_constraints(&input, &mut violations);
    validate_ports(&input, &mut violations);
    validate_environment(&input, &mut violations);
    validate_resources(&input, &mut violations);
    validate_config(&input, &mut violations);
    validate_cross_field_constraints(&input, &mut violations);
    if !violations.is_empty() {
        return violations.into_response();
    }

    // Scope (`deployments:write`) is enforced centrally by the auth middleware.
    // The namespace boundary is this deployment's target namespace: a
    // namespace-scoped PAT may only deploy into a namespace it is scoped to.
    // Checked after validation (so the namespace is well-formed) but before any
    // mutation. A human session passes through unconditionally.
    if let Err(resp) = require_namespace(&auth.source, &input.namespace) {
        return resp;
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
        deployments::find_active_by_namespace_name(&pool, &input.namespace, &input.name).await;

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

                // A published host port can be bound by only one container at a
                // time. Rolling update creates the new container *before*
                // draining the old one, so the new bind collides with the old
                // ("port is already allocated") and the deployment loops in
                // instance_creation_failed. For these deployments we must
                // recreate (drop old, then create new) instead — a brief
                // downtime, but deterministic and loop-free.
                let publishes_host_port = input.ports.iter().any(|p| p.published > 0);

                // Rolling update: keep old deployment running if conditions are met
                if !params.force
                    && has_health_checks
                    && deployments_list.len() == 1
                    && !publishes_host_port
                {
                    let existing = &deployments_list[0];
                    info!(
                        "Rolling update: keeping deployment {} running as parent",
                        existing.id
                    );
                    rolling_parent_id = Some(existing.id.clone());
                } else {
                    // Immediate replace. Pick the most specific reason so
                    // operators can fix the root cause: `force=true` is a
                    // deliberate caller choice, the others are config gaps,
                    // and `host_port_published` is the rolling-incompatible case.
                    replace_reason = Some(if params.force {
                        "force"
                    } else if !has_health_checks {
                        "no_health_checks"
                    } else if deployments_list.len() > 1 {
                        "multiple_active_deployments"
                    } else {
                        "host_port_published"
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

            // Cloud Hypervisor silently ignores some fields that Docker
            // honours (no registry pull, sizing from `limits` only). Rather
            // than drop them without a trace, surface a single warning event
            // naming exactly what won't take effect, so an operator isn't left
            // wondering why their `requests`/registry creds did nothing.
            if deployment.runtime == "cloud-hypervisor" {
                let ignored = cloud_hypervisor_ignored_fields(&deployment);
                if !ignored.is_empty() {
                    let _ = deployment_event::log_event(
                        &pool,
                        deployment.id.clone(),
                        "warning",
                        format!(
                            "These fields are ignored by the cloud-hypervisor runtime and have no effect: {}",
                            ignored.join(", ")
                        ),
                        "api",
                        Some("cloud_hypervisor_ignored_fields"),
                    )
                    .await;
                }
            }

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
                    "host_port_published" => format!(
                        "Replaced {} immediately because it publishes a host port — rolling update would collide on the port (it creates the new container before stopping the old), so Ring recreated it instead (brief downtime)",
                        replaced
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

            let _ = audit_log::record(
                &pool,
                Some(&auth.user.id),
                "create",
                "deployment",
                &input.name,
                Some(&input.namespace),
            )
            .await;

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

        assert_eq!(response.status_code(), StatusCode::UNPROCESSABLE_ENTITY);
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

        assert_eq!(response.status_code(), StatusCode::UNPROCESSABLE_ENTITY);
        let body: serde_json::Value = response.json();
        assert!(
            body["detail"]
                .as_str()
                .unwrap()
                .contains("custom commands are not supported"),
            "unexpected error: {}",
            body["detail"]
        );
    }

    #[tokio::test]
    async fn create_rejects_shared_rw_volume_with_replicas() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post("/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "runtime": "docker",
                "name": "db-scaled-rw",
                "namespace": "ring",
                "image": "couchdb:latest",
                "replicas": 2,
                "volumes": [
                    {
                        "type": "volume",
                        "source": "db-data",
                        "destination": "/opt/couchdb/data",
                        "driver": "local",
                        "permission": "rw"
                    }
                ]
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::UNPROCESSABLE_ENTITY);
        let body: serde_json::Value = response.json();
        assert!(
            body["detail"].as_str().unwrap().contains("corrupts data"),
            "unexpected error: {}",
            body["detail"]
        );
    }

    #[tokio::test]
    async fn create_accepts_shared_ro_volume_with_replicas() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post("/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "runtime": "docker",
                "name": "app-scaled-ro",
                "namespace": "ring",
                "image": "nginx:latest",
                "replicas": 2,
                "volumes": [
                    {
                        "type": "volume",
                        "source": "assets",
                        "destination": "/usr/share/nginx/html",
                        "driver": "local",
                        "permission": "ro"
                    }
                ]
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn create_accepts_rw_volume_with_single_replica() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post("/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "runtime": "docker",
                "name": "db-single-rw",
                "namespace": "ring",
                "image": "couchdb:latest",
                "replicas": 1,
                "volumes": [
                    {
                        "type": "volume",
                        "source": "db-data",
                        "destination": "/opt/couchdb/data",
                        "driver": "local",
                        "permission": "rw"
                    }
                ]
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::CREATED);
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

        assert_eq!(response.status_code(), StatusCode::UNPROCESSABLE_ENTITY);
        let body: serde_json::Value = response.json();
        assert!(
            body["detail"]
                .as_str()
                .unwrap()
                .contains("absolute path to a raw disk image"),
            "unexpected error: {}",
            body["detail"]
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

        assert_eq!(response.status_code(), StatusCode::UNPROCESSABLE_ENTITY);
        let error_body: serde_json::Value = response.json();
        assert!(
            error_body["detail"]
                .as_str()
                .unwrap()
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

        assert_eq!(response.status_code(), StatusCode::UNPROCESSABLE_ENTITY);
        let error_body: serde_json::Value = response.json();
        assert!(
            error_body["detail"]
                .as_str()
                .unwrap()
                .contains("source cannot be empty")
        );
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

        assert_eq!(response.status_code(), StatusCode::UNPROCESSABLE_ENTITY);
        let error_body: serde_json::Value = response.json();
        assert!(
            error_body["detail"]
                .as_str()
                .unwrap()
                .contains("source is required for config volumes")
                || error_body["detail"]
                    .as_str()
                    .unwrap()
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

        assert_eq!(response.status_code(), StatusCode::UNPROCESSABLE_ENTITY);
        let error_body: serde_json::Value = response.json();
        assert!(
            error_body["detail"]
                .as_str()
                .unwrap()
                .contains("source cannot be empty")
                || error_body["detail"]
                    .as_str()
                    .unwrap()
                    .contains("key cannot be empty")
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

        assert_eq!(response.status_code(), StatusCode::UNPROCESSABLE_ENTITY);
        let error_body: serde_json::Value = response.json();
        assert!(
            error_body["detail"]
                .as_str()
                .unwrap()
                .contains("destination cannot be empty")
        );
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

        assert_eq!(response.status_code(), StatusCode::UNPROCESSABLE_ENTITY);
        let error_body: serde_json::Value = response.json();
        let detail = error_body["detail"].as_str().unwrap_or("");
        assert!(detail.contains("source") || detail.contains("destination"));
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

        assert_eq!(response.status_code(), StatusCode::UNPROCESSABLE_ENTITY);
        let error_body: serde_json::Value = response.json();
        assert!(
            error_body["detail"]
                .as_str()
                .unwrap()
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

        assert_eq!(response.status_code(), StatusCode::UNPROCESSABLE_ENTITY);
        let error_body: serde_json::Value = response.json();
        assert!(
            error_body["detail"]
                .as_str()
                .unwrap()
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

        assert_eq!(response.status_code(), StatusCode::UNPROCESSABLE_ENTITY);
        let error_body: serde_json::Value = response.json();
        assert!(
            error_body["detail"]
                .as_str()
                .unwrap()
                .contains("source cannot be empty")
        );
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

        assert_eq!(response.status_code(), StatusCode::UNPROCESSABLE_ENTITY);
        let error_body: serde_json::Value = response.json();
        assert!(
            error_body["detail"]
                .as_str()
                .unwrap()
                .contains("key cannot be empty")
        );
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

        assert_eq!(response.status_code(), StatusCode::UNPROCESSABLE_ENTITY);
        let error_body: serde_json::Value = response.json();
        assert!(
            error_body["detail"]
                .as_str()
                .unwrap()
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

        assert_eq!(response.status_code(), StatusCode::UNPROCESSABLE_ENTITY);
        let error_body: serde_json::Value = response.json();
        assert!(
            error_body["detail"]
                .as_str()
                .unwrap()
                .contains("source cannot be empty")
        );
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

        assert_eq!(response.status_code(), StatusCode::UNPROCESSABLE_ENTITY);
        let error_body: serde_json::Value = response.json();
        assert!(
            error_body["detail"]
                .as_str()
                .unwrap()
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
                || response.status_code() == StatusCode::UNPROCESSABLE_ENTITY
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
    async fn host_port_forces_recreate_instead_of_rolling() {
        let (pool, app) = new_test_app_with_pool().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        // Initial deployment: health checks AND a published host port. Rolling
        // would normally apply (HC present, single active), but the host port
        // makes it rolling-incompatible.
        let response = server
            .post("/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "runtime": "docker",
                "name": "edge-app",
                "namespace": "edge-ns",
                "image": "nginx:1.0",
                "ports": [{"published": 18080, "target": 80}],
                "health_checks": [{"type": "tcp", "port": 80, "interval": "10s", "timeout": "5s", "on_failure": "restart"}]
            }))
            .await;
        assert_eq!(response.status_code(), StatusCode::CREATED);
        let first: serde_json::Value = response.json();
        let first_id = first["id"].as_str().unwrap().to_string();

        sqlx::query("UPDATE deployment SET status = 'running' WHERE id = ?")
            .bind(&first_id)
            .execute(&pool)
            .await
            .unwrap();

        // Re-apply (same name, new image, same host port). Must recreate, not roll.
        let response = server
            .post("/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "runtime": "docker",
                "name": "edge-app",
                "namespace": "edge-ns",
                "image": "nginx:2.0",
                "ports": [{"published": 18080, "target": 80}],
                "health_checks": [{"type": "tcp", "port": 80, "interval": "10s", "timeout": "5s", "on_failure": "restart"}]
            }))
            .await;
        assert_eq!(response.status_code(), StatusCode::CREATED);

        // Parent must be wiped (recreate), not kept running as a rolling parent.
        let parent: serde_json::Value = server
            .get(&format!("/deployments/{}", first_id))
            .add_header("Authorization", format!("Bearer {}", token))
            .await
            .json();
        assert_eq!(
            parent["status"], "deleted",
            "host-port deployment must recreate, not roll"
        );

        // The new deployment carries the host_port_published reason.
        let deployments: Vec<serde_json::Value> = server
            .get("/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .await
            .json();
        let new_id = deployments
            .iter()
            .find(|d| d["name"] == "edge-app" && d["id"].as_str() != Some(&first_id))
            .expect("new deployment must exist")["id"]
            .as_str()
            .unwrap()
            .to_string();
        let event: (String, String) = sqlx::query_as(
            "SELECT level, message FROM deployment_event WHERE deployment_id = ? AND reason = 'force_replace'",
        )
        .bind(&new_id)
        .fetch_one(&pool)
        .await
        .expect("recreate event must be logged");
        assert_eq!(event.0, "warning");
        assert!(event.1.contains("host port"), "got message: {}", event.1);
    }

    #[tokio::test]
    async fn rolling_update_without_host_port_still_rolls() {
        let (pool, app) = new_test_app_with_pool().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        // Non-regression: HC present, single active, NO host port → still rolling.
        let response = server
            .post("/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "runtime": "docker",
                "name": "noport-app",
                "namespace": "noport-ns",
                "image": "nginx:1.0",
                "health_checks": [{"type": "tcp", "port": 80, "interval": "10s", "timeout": "5s", "on_failure": "restart"}]
            }))
            .await;
        assert_eq!(response.status_code(), StatusCode::CREATED);
        let first_id = response.json::<serde_json::Value>()["id"]
            .as_str()
            .unwrap()
            .to_string();

        sqlx::query("UPDATE deployment SET status = 'running' WHERE id = ?")
            .bind(&first_id)
            .execute(&pool)
            .await
            .unwrap();

        let response = server
            .post("/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "runtime": "docker",
                "name": "noport-app",
                "namespace": "noport-ns",
                "image": "nginx:2.0",
                "health_checks": [{"type": "tcp", "port": 80, "interval": "10s", "timeout": "5s", "on_failure": "restart"}]
            }))
            .await;
        assert_eq!(response.status_code(), StatusCode::CREATED);

        // Parent kept running → rolling, not recreate.
        let parent: serde_json::Value = server
            .get(&format!("/deployments/{}", first_id))
            .add_header("Authorization", format!("Bearer {}", token))
            .await
            .json();
        assert_eq!(parent["status"], "running");
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

        assert_eq!(response.status_code(), StatusCode::UNPROCESSABLE_ENTITY);
        let body: serde_json::Value = response.json();
        // Match on the stable `code` slug instead of human text — the
        // wording of `message` may evolve without breaking clients.
        let codes: Vec<String> = body["violations"]
            .as_array()
            .unwrap_or(&vec![])
            .iter()
            .map(|v| v["code"].as_str().unwrap_or("").to_string())
            .collect();
        assert!(
            codes.contains(&"deployment.ports.host_network_conflict".to_string()),
            "expected deployment.ports.host_network_conflict, got {:?}",
            codes
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

        assert_eq!(response.status_code(), StatusCode::UNPROCESSABLE_ENTITY);
        let body: serde_json::Value = response.json();
        assert!(
            body["detail"].as_str().unwrap().contains("replicas"),
            "unexpected message: {}",
            body["detail"]
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

        assert_eq!(response.status_code(), StatusCode::UNPROCESSABLE_ENTITY);
        let body: serde_json::Value = response.json();
        assert!(
            body["detail"].as_str().unwrap().contains("docker runtime"),
            "unexpected message: {}",
            body["detail"]
        );
    }

    #[tokio::test]
    async fn create_with_bridge_network_mode_explicit() {
        // bridge mode is the existing default — declaring it explicitly
        // must not change anything. `ports` and `replicas: 1` remain
        // a valid pair (replicas > 1 with ports is rejected separately,
        // see `create_rejects_ports_with_replicas_above_one`).
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
                "replicas": 1
            }))
            .await;

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

    // ────────────────────────────────────────────────────────────────────────
    // Property paths
    // ────────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn create_violation_paths_use_jsonpath_for_lists() {
        // A violation on volumes[N].source must surface that exact path so
        // the operator can point at the right entry in their manifest. The
        // path adapter in `api/validation.rs` is generic; this test pins
        // the JSONPath convention for any nested-list validation.
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

        assert_eq!(response.status_code(), StatusCode::UNPROCESSABLE_ENTITY);
        let body: serde_json::Value = response.json();
        let paths: Vec<&str> = body["violations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["property_path"].as_str().unwrap())
            .collect();
        assert!(
            paths.contains(&"volumes[0].source"),
            "expected JSONPath `volumes[0].source`, got {:?}",
            paths
        );
    }

    // ────────────────────────────────────────────────────────────────────────
    // Port validation
    // ────────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn create_rejects_port_zero() {
        // 0 is reserved by the kernel — Docker uses it to mean "pick any
        // free port", which is not what the user typed. Reject explicitly.
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
                "ports": [{ "published": 0, "target": 80 }]
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::UNPROCESSABLE_ENTITY);
        let body: serde_json::Value = response.json();
        let v = body["violations"].as_array().unwrap();
        let codes: Vec<&str> = v.iter().map(|x| x["code"].as_str().unwrap()).collect();
        let paths: Vec<&str> = v
            .iter()
            .map(|x| x["property_path"].as_str().unwrap())
            .collect();
        assert!(
            codes.contains(&"deployment.ports.published.out_of_range"),
            "got codes {:?}",
            codes
        );
        assert!(
            paths.contains(&"ports[0].published"),
            "got paths {:?}",
            paths
        );
    }

    #[tokio::test]
    async fn create_rejects_target_zero() {
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
                "ports": [{ "published": 8080, "target": 0 }]
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::UNPROCESSABLE_ENTITY);
        let body: serde_json::Value = response.json();
        let v = body["violations"].as_array().unwrap();
        let codes: Vec<&str> = v.iter().map(|x| x["code"].as_str().unwrap()).collect();
        let paths: Vec<&str> = v
            .iter()
            .map(|x| x["property_path"].as_str().unwrap())
            .collect();
        assert!(
            codes.contains(&"deployment.ports.target.out_of_range"),
            "got codes {:?}",
            codes
        );
        assert!(paths.contains(&"ports[0].target"), "got paths {:?}", paths);
    }

    #[tokio::test]
    async fn create_rejects_duplicate_published_ports() {
        // Two entries publishing the same host port can't both bind.
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
                    { "published": 8080, "target": 81 }
                ]
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::UNPROCESSABLE_ENTITY);
        let body: serde_json::Value = response.json();
        let codes: Vec<String> = body["violations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["code"].as_str().unwrap().to_string())
            .collect();
        assert!(
            codes.contains(&"deployment.ports.published.duplicate".to_string()),
            "got {:?}",
            codes
        );
    }

    #[tokio::test]
    async fn create_rejects_invalid_host_ip() {
        // host_ip is forwarded verbatim to Docker / socat. A non-IP value
        // must be caught at the API with a field-scoped path, not as an
        // opaque runtime event later.
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
                "ports": [{ "published": 8080, "target": 80, "host_ip": "not-an-ip" }]
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::UNPROCESSABLE_ENTITY);
        let body: serde_json::Value = response.json();
        let v = body["violations"].as_array().unwrap();
        let codes: Vec<&str> = v.iter().map(|x| x["code"].as_str().unwrap()).collect();
        let paths: Vec<&str> = v
            .iter()
            .map(|x| x["property_path"].as_str().unwrap())
            .collect();
        assert!(
            codes.contains(&"deployment.ports.host_ip.invalid"),
            "got codes {:?}",
            codes
        );
        assert!(paths.contains(&"ports[0].host_ip"), "got paths {:?}", paths);
    }

    #[tokio::test]
    async fn create_accepts_valid_host_ip() {
        // A well-formed loopback address must pass validation untouched.
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
                "ports": [{ "published": 8080, "target": 80, "host_ip": "127.0.0.1" }]
            }))
            .await;

        assert_ne!(
            response.status_code(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "valid host_ip must not trip validation: {:?}",
            response.json::<serde_json::Value>()
        );
    }

    // ────────────────────────────────────────────────────────────────────────
    // Environment / Resources / Config rules
    // ────────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn create_rejects_invalid_env_var_name() {
        // POSIX/Docker env var names: [A-Za-z_][A-Za-z0-9_]*. Names with
        // spaces, dots, or dashes silently become unusable variables in
        // the container shell. Reject up front.
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
                "environment": {
                    "bad-name": "value",
                    "1NOT_ALLOWED": "value",
                    "GOOD_ONE": "ok"
                }
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::UNPROCESSABLE_ENTITY);
        let body: serde_json::Value = response.json();
        let codes: Vec<String> = body["violations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["code"].as_str().unwrap().to_string())
            .collect();
        assert!(
            codes
                .iter()
                .filter(|c| *c == "deployment.environment.key.invalid")
                .count()
                >= 2,
            "expected at least 2 invalid env keys, got {:?}",
            codes
        );
    }

    #[tokio::test]
    async fn create_rejects_invalid_cpu_string() {
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
                "resources": {
                    "limits": { "cpu": "notanumber" }
                }
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::UNPROCESSABLE_ENTITY);
        let body: serde_json::Value = response.json();
        let codes: Vec<String> = body["violations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["code"].as_str().unwrap().to_string())
            .collect();
        assert!(
            codes.contains(&"deployment.resources.limits.cpu.invalid".to_string()),
            "got {:?}",
            codes
        );
    }

    #[tokio::test]
    async fn create_rejects_invalid_memory_string() {
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
                "resources": {
                    "requests": { "memory": "lots" }
                }
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::UNPROCESSABLE_ENTITY);
        let body: serde_json::Value = response.json();
        let codes: Vec<String> = body["violations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["code"].as_str().unwrap().to_string())
            .collect();
        assert!(
            codes.contains(&"deployment.resources.requests.memory.invalid".to_string()),
            "got {:?}",
            codes
        );
    }

    #[tokio::test]
    async fn create_rejects_unknown_image_pull_policy() {
        // Docker accepts: Always, IfNotPresent, Never. Anything else is
        // silently ignored at runtime — fail at API time instead.
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
                "config": { "image_pull_policy": "Maybe" }
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::UNPROCESSABLE_ENTITY);
        let body: serde_json::Value = response.json();
        let codes: Vec<String> = body["violations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["code"].as_str().unwrap().to_string())
            .collect();
        assert!(
            codes.contains(&"deployment.config.image_pull_policy.unsupported".to_string()),
            "got {:?}",
            codes
        );
    }

    // ────────────────────────────────────────────────────────────────────────
    // Cross-field rules
    // ────────────────────────────────────────────────────────────────────────
    //
    // These rules catch combinations that are syntactically valid but
    // semantically broken — the apply would silently produce a non-working
    // deployment. They surface BOTH affected fields so the user can pick
    // which one to change, as per the convention agreed for cross-field
    // violations.

    #[tokio::test]
    async fn create_rejects_ports_with_replicas_above_one() {
        // Two replicas binding the same host port collide. Either reduce
        // replicas or drop the ports — surface both options.
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
                "replicas": 3,
                "ports": [{ "published": 80, "target": 80 }]
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::UNPROCESSABLE_ENTITY);
        let body: serde_json::Value = response.json();
        let codes: Vec<String> = body["violations"]
            .as_array()
            .unwrap_or(&vec![])
            .iter()
            .map(|v| v["code"].as_str().unwrap_or("").to_string())
            .collect();
        assert!(
            codes.contains(&"deployment.ports.replicas_conflict".to_string()),
            "missing ports code, got {:?}",
            codes
        );
        assert!(
            codes.contains(&"deployment.replicas.ports_conflict".to_string()),
            "missing replicas code, got {:?}",
            codes
        );
    }

    #[tokio::test]
    async fn create_rejects_job_with_replicas_above_one() {
        // A job is one-shot by definition. replicas > 1 is meaningless.
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post("/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "runtime": "docker",
                "kind": "job",
                "name": "batch",
                "namespace": "ring",
                "image": "busybox:latest",
                "replicas": 4
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::UNPROCESSABLE_ENTITY);
        let body: serde_json::Value = response.json();
        let codes: Vec<String> = body["violations"]
            .as_array()
            .unwrap_or(&vec![])
            .iter()
            .map(|v| v["code"].as_str().unwrap_or("").to_string())
            .collect();
        assert!(
            codes.contains(&"deployment.replicas.job_must_be_one".to_string()),
            "expected job/replicas violation, got {:?}",
            codes
        );
    }

    #[tokio::test]
    async fn create_rejects_job_with_readiness_check() {
        // Readiness gates a rolling update — but jobs don't roll, they
        // run once and exit. A readiness check on a job is a config gap.
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response: TestResponse = server
            .post("/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "runtime": "docker",
                "kind": "job",
                "name": "batch",
                "namespace": "ring",
                "image": "busybox:latest",
                "health_checks": [{
                    "type": "tcp",
                    "port": 8080,
                    "interval": "5s",
                    "timeout": "2s",
                    "on_failure": "restart",
                    "readiness": true
                }]
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::UNPROCESSABLE_ENTITY);
        let body: serde_json::Value = response.json();
        let codes: Vec<String> = body["violations"]
            .as_array()
            .unwrap_or(&vec![])
            .iter()
            .map(|v| v["code"].as_str().unwrap_or("").to_string())
            .collect();
        assert!(
            codes.contains(&"deployment.health_checks.job_readiness_unsupported".to_string()),
            "expected job/readiness violation, got {:?}",
            codes
        );
    }
}
