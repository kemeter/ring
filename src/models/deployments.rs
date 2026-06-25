use crate::api::dto::deployment::DeploymentVolume;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::fmt;

pub(crate) const MAX_RESTART_COUNT: u32 = 5;

/// All variants serialize to snake_case, both on the wire (serde JSON) and
/// in the SQLite `deployment.status` column (Display). Before this change,
/// lifecycle states were lowercase (`running`, …) while error states were
/// PascalCase (`CrashLoopBackOff`, …). The mismatch silently dropped rows
/// from string-matching filters — see migration `20220101000015` for the DB
/// rewrite and the PR description for the full trace.
///
/// **Breaking change** for any external consumer that parsed the JSON API
/// or `ring deployment list` output expecting the PascalCase form. Mapping:
/// `CrashLoopBackOff` → `crash_loop_back_off`, `ImagePullBackOff` →
/// `image_pull_back_off`, etc.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DeploymentStatus {
    Pending,
    Creating,
    Running,
    Completed,
    Failed,
    Deleted,
    CrashLoopBackOff,
    ImagePullBackOff,
    CreateContainerError,
    NetworkError,
    ConfigError,
    FileSystemError,
    InsufficientResources,
    Error,
}

impl fmt::Display for DeploymentStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pending => write!(f, "pending"),
            Self::Creating => write!(f, "creating"),
            Self::Running => write!(f, "running"),
            Self::Completed => write!(f, "completed"),
            Self::Failed => write!(f, "failed"),
            Self::Deleted => write!(f, "deleted"),
            Self::CrashLoopBackOff => write!(f, "crash_loop_back_off"),
            Self::ImagePullBackOff => write!(f, "image_pull_back_off"),
            Self::CreateContainerError => write!(f, "create_container_error"),
            Self::NetworkError => write!(f, "network_error"),
            Self::ConfigError => write!(f, "config_error"),
            Self::FileSystemError => write!(f, "file_system_error"),
            Self::InsufficientResources => write!(f, "insufficient_resources"),
            Self::Error => write!(f, "error"),
        }
    }
}

impl std::str::FromStr for DeploymentStatus {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "pending" => Ok(Self::Pending),
            "creating" => Ok(Self::Creating),
            "running" => Ok(Self::Running),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "deleted" => Ok(Self::Deleted),
            "crash_loop_back_off" => Ok(Self::CrashLoopBackOff),
            "image_pull_back_off" => Ok(Self::ImagePullBackOff),
            "create_container_error" => Ok(Self::CreateContainerError),
            "network_error" => Ok(Self::NetworkError),
            "config_error" => Ok(Self::ConfigError),
            "file_system_error" => Ok(Self::FileSystemError),
            "insufficient_resources" => Ok(Self::InsufficientResources),
            "error" => Ok(Self::Error),
            other => Err(format!("Unknown deployment status: {}", other)),
        }
    }
}

impl DeploymentStatus {
    /// Every variant, in declaration order. Used to emit a metric series per
    /// status even when its count is zero — a Prometheus series that vanishes
    /// between scrapes breaks alerts written against it, so all statuses are
    /// always present.
    pub(crate) const fn all() -> [DeploymentStatus; 14] {
        [
            Self::Pending,
            Self::Creating,
            Self::Running,
            Self::Completed,
            Self::Failed,
            Self::Deleted,
            Self::CrashLoopBackOff,
            Self::ImagePullBackOff,
            Self::CreateContainerError,
            Self::NetworkError,
            Self::ConfigError,
            Self::FileSystemError,
            Self::InsufficientResources,
            Self::Error,
        ]
    }
}

#[cfg(test)]
mod status_roundtrip_tests {
    use super::DeploymentStatus;
    use std::str::FromStr;

    /// Every variant must round-trip Display ↔ FromStr ↔ serde with the
    /// exact same string. Catches any future divergence between the three
    /// representations (the lifecycle/error casing mismatch we just fixed
    /// went undetected for months because there was no such test).
    #[test]
    fn every_variant_round_trips() {
        for s in DeploymentStatus::all() {
            let txt = s.to_string();
            // snake_case: lowercase + underscores only.
            assert!(
                txt.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                "{:?} must Display as snake_case, got {:?}",
                s,
                txt
            );
            let parsed = DeploymentStatus::from_str(&txt).expect("must parse");
            assert_eq!(parsed, s, "{:?} round-trip via Display/FromStr", s);
            // serde JSON wraps strings in quotes.
            let json = serde_json::to_string(&s).expect("must serialize");
            assert_eq!(json, format!("\"{}\"", txt), "serde matches Display");
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct UserConfig {
    pub id: Option<u32>,
    pub group: Option<u32>,
    pub privileged: Option<bool>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub(crate) struct DeploymentConfig {
    #[serde(default = "default_image_pull_policy")]
    pub(crate) image_pull_policy: String,
    pub(crate) server: Option<String>,
    pub(crate) username: Option<String>,
    pub(crate) password: Option<String>,
    pub(crate) user: Option<UserConfig>,
    /// Opt into resolving registry credentials from the host's Docker config
    /// (`~/.docker/config.json`) instead of inlining `server`/`username`/
    /// `password`. The server must also authorize it via
    /// `use_host_registry_auth`; this flag only *activates* it per-deployment.
    /// Skipped from serialization when `false` so existing config payloads keep
    /// their byte-for-byte shape (no DB migration, no API surface change).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) use_host_auth: bool,
    /// Name of a `Secret` (same namespace) holding registry credentials as a
    /// Docker `config.json` payload (`dockerconfigjson`). The scheduler decrypts
    /// it and fills `server`/`username`/`password` before the runtime pulls, so
    /// the secret is never inlined in the manifest, the database, or the API.
    /// Mutually exclusive with inline credentials and `use_host_auth`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) image_pull_secret: Option<String>,
}

/// Transport protocol for a published port. TCP is the default, preserving the
/// shape of manifests written before UDP forwarding existed.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "lowercase")]
pub(crate) enum PortProtocol {
    #[default]
    Tcp,
    Udp,
}

impl PortProtocol {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            PortProtocol::Tcp => "tcp",
            PortProtocol::Udp => "udp",
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub(crate) struct DeploymentPort {
    pub(crate) published: u16,
    pub(crate) target: u16,
    /// Host interface to bind the published port on. Defaults to `0.0.0.0`
    /// (all interfaces) when omitted, preserving prior behavior. Set to
    /// `127.0.0.1` to expose the port on loopback only.
    #[serde(default)]
    pub(crate) host_ip: Option<String>,
    /// Transport protocol (`tcp` by default, or `udp`).
    #[serde(default)]
    pub(crate) protocol: PortProtocol,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub(crate) enum NetworkMode {
    #[default]
    Bridge,
    Host,
}

impl NetworkMode {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Bridge => "bridge",
            Self::Host => "host",
        }
    }
}

impl std::str::FromStr for NetworkMode {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "bridge" => Ok(Self::Bridge),
            "host" => Ok(Self::Host),
            other => Err(format!("Unknown network mode: {}", other)),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub(crate) struct NetworkConfig {
    #[serde(default)]
    pub(crate) mode: NetworkMode,
}

pub(crate) fn default_image_pull_policy() -> String {
    "Always".to_string()
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(untagged)]
pub(crate) enum EnvValue {
    Plain(String),
    SecretRef {
        #[serde(rename = "secretRef")]
        secret_ref: String,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub(crate) struct ResourceSpec {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) cpu: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) memory: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub(crate) struct Resource {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) limits: Option<ResourceSpec>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) requests: Option<ResourceSpec>,
}

pub(crate) fn parse_cpu_string(s: &str) -> Result<i64, String> {
    let s = s.trim();

    if let Some(stripped) = s.strip_suffix('m') {
        let millis: f64 = stripped
            .parse()
            .map_err(|_| format!("Invalid CPU millicores value: {}", s))?;
        return Ok((millis * 1_000_000.0) as i64);
    }

    let cores: f64 = s.parse().map_err(|_| format!("Invalid CPU value: {}", s))?;
    Ok((cores * 1_000_000_000.0) as i64)
}

pub(crate) fn parse_memory_string(s: &str) -> Result<i64, String> {
    let s = s.trim();

    if let Ok(bytes) = s.parse::<i64>() {
        return Ok(bytes);
    }

    let (suffix, multiplier): (&str, i64) = if s.ends_with("Ti") {
        ("Ti", 1024i64 * 1024 * 1024 * 1024)
    } else if s.ends_with("Gi") {
        ("Gi", 1024i64 * 1024 * 1024)
    } else if s.ends_with("Mi") {
        ("Mi", 1024i64 * 1024)
    } else if s.ends_with("Ki") {
        ("Ki", 1024i64)
    } else if s.ends_with('T') {
        ("T", 1_000_000_000_000i64)
    } else if s.ends_with('G') {
        ("G", 1_000_000_000i64)
    } else if s.ends_with('M') {
        ("M", 1_000_000i64)
    } else if s.ends_with('K') {
        ("K", 1_000i64)
    } else {
        return Err(format!("Invalid memory format: {}", s));
    };

    let num_str = &s[..s.len() - suffix.len()];
    let value: f64 = num_str
        .parse()
        .map_err(|_| format!("Invalid numeric value in memory string: {}", s))?;

    Ok((value * multiplier as f64) as i64)
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub(crate) struct Deployment {
    pub(crate) id: String,
    pub(crate) created_at: String,
    pub(crate) updated_at: Option<String>,
    pub(crate) status: DeploymentStatus,
    pub(crate) restart_count: u32,
    pub(crate) namespace: String,
    pub(crate) name: String,
    pub(crate) image: String,
    pub(crate) config: Option<DeploymentConfig>,
    pub(crate) runtime: String,
    pub(crate) kind: String,
    pub(crate) replicas: u32,
    pub(crate) command: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub(crate) instances: Vec<String>,
    pub(crate) labels: HashMap<String, String>,
    pub(crate) environment: HashMap<String, EnvValue>,
    pub(crate) volumes: String,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub(crate) health_checks: Vec<crate::models::health_check::HealthCheck>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub(crate) resources: Option<Resource>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub(crate) image_digest: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub(crate) ports: Vec<DeploymentPort>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    #[serde(skip_deserializing)]
    pub(crate) pending_events: Vec<crate::models::deployment_event::DeploymentEvent>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub(crate) parent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub(crate) network: Option<NetworkConfig>,
}

impl Deployment {
    pub fn emit_event(
        &mut self,
        level: &str,
        message: String,
        component: &str,
        reason: Option<&str>,
    ) {
        let event = crate::models::deployment_event::DeploymentEvent::new(
            self.id.clone(),
            level,
            message,
            component,
            reason,
        );
        self.pending_events.push(event);
    }
}

#[derive(sqlx::FromRow)]
struct DeploymentRow {
    id: String,
    created_at: String,
    updated_at: Option<String>,
    status: String,
    restart_count: i32,
    namespace: String,
    name: String,
    image: String,
    command: String,
    config: Option<String>,
    runtime: String,
    kind: String,
    replicas: i32,
    labels: String,
    environment: String,
    volumes: String,
    health_checks: Option<String>,
    resources: Option<String>,
    image_digest: Option<String>,
    parent_id: Option<String>,
    ports: Option<String>,
    network_mode: Option<String>,
}

fn parse_environment(json_str: &str, deployment_id: &str) -> HashMap<String, EnvValue> {
    // Try new format first (HashMap<String, EnvValue>)
    if let Ok(env) = serde_json::from_str::<HashMap<String, EnvValue>>(json_str) {
        return env;
    }

    // Fallback to old format (HashMap<String, String>) for backwards compatibility
    match serde_json::from_str::<HashMap<String, String>>(json_str) {
        Ok(old_format) => old_format
            .into_iter()
            .map(|(k, v)| (k, EnvValue::Plain(v)))
            .collect(),
        Err(e) => {
            warn!(
                "Failed to deserialize environment for deployment {}: {}",
                deployment_id, e
            );
            HashMap::new()
        }
    }
}

impl From<DeploymentRow> for Deployment {
    fn from(row: DeploymentRow) -> Self {
        let id = row.id;
        Deployment {
            id: id.clone(),
            created_at: row.created_at,
            updated_at: row.updated_at,
            status: row.status.parse().unwrap_or(DeploymentStatus::Error),
            restart_count: row.restart_count as u32,
            namespace: row.namespace,
            name: row.name,
            image: row.image,
            config: row.config.and_then(|c| serde_json::from_str(&c).ok()),
            runtime: row.runtime,
            kind: row.kind,
            replicas: row.replicas as u32,
            command: serde_json::from_str(&row.command).unwrap_or_else(|e| {
                warn!("Failed to deserialize command for deployment {}: {}", id, e);
                Vec::new()
            }),
            instances: vec![],
            labels: serde_json::from_str(&row.labels).unwrap_or_else(|e| {
                warn!("Failed to deserialize labels for deployment {}: {}", id, e);
                HashMap::new()
            }),
            environment: parse_environment(&row.environment, &id),
            volumes: row.volumes,
            health_checks: row
                .health_checks
                .filter(|s| !s.is_empty())
                .map(|s| {
                    serde_json::from_str(&s).unwrap_or_else(|e| {
                        warn!(
                            "Failed to deserialize health_checks for deployment {}: {}",
                            id, e
                        );
                        Vec::new()
                    })
                })
                .unwrap_or_default(),
            resources: row
                .resources
                .filter(|s| !s.is_empty())
                .and_then(|s| serde_json::from_str(&s).ok()),
            image_digest: row.image_digest,
            ports: row
                .ports
                .filter(|s| !s.is_empty())
                .map(|s| {
                    serde_json::from_str(&s).unwrap_or_else(|e| {
                        warn!("Failed to deserialize ports for deployment {}: {}", id, e);
                        Vec::new()
                    })
                })
                .unwrap_or_default(),
            pending_events: vec![],
            parent_id: row.parent_id,
            network: row.network_mode.as_deref().and_then(|s| {
                use std::str::FromStr;
                NetworkMode::from_str(s)
                    .map(|mode| NetworkConfig { mode })
                    .map_err(|e| {
                        warn!(
                            "Failed to parse network_mode '{}' for deployment {}: {}",
                            s, id, e
                        );
                        e
                    })
                    .ok()
            }),
        }
    }
}

const SELECT_COLUMNS: &str = "
    id, created_at, updated_at, status, restart_count,
    namespace, name, image, command, config, runtime, kind,
    replicas, labels, environment, volumes, health_checks, resources, image_digest, parent_id, ports, network_mode
";

const ALLOWED_FILTER_COLUMNS: &[&str] = &["namespace", "status", "kind"];

pub(crate) async fn find_all(
    pool: &SqlitePool,
    filters: HashMap<String, Vec<String>>,
) -> Result<Vec<Deployment>, sqlx::Error> {
    let base_query = format!("SELECT {} FROM deployment", SELECT_COLUMNS);
    let (query, values) =
        crate::models::query::build_filtered_query(&base_query, &filters, ALLOWED_FILTER_COLUMNS);

    let mut q = sqlx::query_as::<_, DeploymentRow>(&query);
    for val in &values {
        q = q.bind(val);
    }

    let rows = q.fetch_all(pool).await?;
    Ok(rows.into_iter().map(Deployment::from).collect())
}

pub(crate) async fn find_active_by_namespace_name(
    pool: &SqlitePool,
    namespace: &str,
    name: &str,
) -> Result<Vec<Deployment>, sqlx::Error> {
    let sql = format!(
        "SELECT {} FROM deployment WHERE namespace = ? AND name = ? AND status <> 'deleted' ORDER BY created_at DESC",
        SELECT_COLUMNS
    );

    let rows = sqlx::query_as::<_, DeploymentRow>(&sql)
        .bind(namespace)
        .bind(name)
        .fetch_all(pool)
        .await?;

    Ok(rows.into_iter().map(Deployment::from).collect())
}

pub(crate) async fn find(pool: &SqlitePool, id: &str) -> Result<Option<Deployment>, sqlx::Error> {
    let sql = format!("SELECT {} FROM deployment WHERE id = ?", SELECT_COLUMNS);

    let row = sqlx::query_as::<_, DeploymentRow>(&sql)
        .bind(id)
        .fetch_optional(pool)
        .await?;

    Ok(row.map(Deployment::from))
}

pub(crate) async fn create(
    pool: &SqlitePool,
    deployment: &Deployment,
) -> Result<Deployment, sqlx::Error> {
    let labels = serde_json::to_string(&deployment.labels).unwrap_or_else(|_| "[]".to_string());
    let environment =
        serde_json::to_string(&deployment.environment).unwrap_or_else(|_| "{}".to_string());
    let config_json = match &deployment.config {
        Some(config) => serde_json::to_string(config).unwrap_or_else(|_| "{}".to_string()),
        None => "{}".to_string(),
    };
    let command_json =
        serde_json::to_string(&deployment.command).unwrap_or_else(|_| "[]".to_string());
    let health_checks_json =
        serde_json::to_string(&deployment.health_checks).unwrap_or_else(|_| "[]".to_string());

    let resources_json = deployment
        .resources
        .as_ref()
        .map(|r| serde_json::to_string(r).unwrap_or_else(|_| "null".to_string()));
    let ports_json = serde_json::to_string(&deployment.ports).unwrap_or_else(|_| "[]".to_string());

    let network_mode = deployment
        .network
        .as_ref()
        .map(|n| n.mode.as_str().to_string());

    sqlx::query(
        "INSERT INTO deployment (
            id, created_at, status, restart_count, namespace, name, image,
            command, config, runtime, kind, replicas, labels, environment, volumes, health_checks, resources, image_digest, parent_id, ports, network_mode
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
    )
    .bind(&deployment.id)
    .bind(&deployment.created_at)
    .bind(deployment.status.to_string())
    .bind(deployment.restart_count as i32)
    .bind(&deployment.namespace)
    .bind(&deployment.name)
    .bind(&deployment.image)
    .bind(&command_json)
    .bind(&config_json)
    .bind(&deployment.runtime)
    .bind(&deployment.kind)
    .bind(deployment.replicas as i32)
    .bind(&labels)
    .bind(&environment)
    .bind(&deployment.volumes)
    .bind(&health_checks_json)
    .bind(&resources_json)
    .bind(&deployment.image_digest)
    .bind(&deployment.parent_id)
    .bind(&ports_json)
    .bind(&network_mode)
    .execute(pool)
    .await?;

    Ok(deployment.clone())
}

pub(crate) async fn update(pool: &SqlitePool, deployment: &Deployment) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE deployment SET status = ?, updated_at = datetime('now'), restart_count = ?, image_digest = ?, parent_id = ? WHERE id = ?"
    )
    .bind(deployment.status.to_string())
    .bind(deployment.restart_count as i32)
    .bind(&deployment.image_digest)
    .bind(&deployment.parent_id)
    .bind(&deployment.id)
    .execute(pool)
    .await?;

    Ok(())
}

pub(crate) async fn find_referencing_secret(
    pool: &SqlitePool,
    namespace: &str,
    secret_name: &str,
) -> Result<Vec<Deployment>, sqlx::Error> {
    // Search for deployments in the same namespace that reference this secret
    let pattern = format!("%\"secretRef\":\"{}\"% ", secret_name);
    let sql = format!(
        "SELECT {} FROM deployment WHERE namespace = ? AND environment LIKE ? AND status NOT IN ('deleted', 'completed', 'failed')",
        SELECT_COLUMNS
    );

    let rows = sqlx::query_as::<_, DeploymentRow>(&sql)
        .bind(namespace)
        .bind(&pattern)
        .fetch_all(pool)
        .await?;

    Ok(rows.into_iter().map(Deployment::from).collect())
}

/// True when `volumes_json` (a deployment's `volumes` column) contains a mount
/// of `type == "volume"` whose `source` is `volume_name`. Distinguishes a real
/// named-volume reference from a bind/config/secret that merely shares the name,
/// which a bare SQL `LIKE` on `source` cannot. A column that fails to parse is
/// treated as "no reference" — it can't be a structured volume mount.
fn deployment_mounts_named_volume(volumes_json: &str, volume_name: &str) -> bool {
    serde_json::from_str::<Vec<DeploymentVolume>>(volumes_json)
        .map(|mounts| {
            mounts.iter().any(|mount| {
                mount.r#type == "volume" && mount.source.as_deref() == Some(volume_name)
            })
        })
        .unwrap_or(false)
}

pub(crate) async fn find_referencing_volume(
    pool: &SqlitePool,
    namespace: &str,
    volume_name: &str,
) -> Result<Vec<Deployment>, sqlx::Error> {
    // A named volume appears in a deployment's `volumes JSON` as
    // {"type":"volume","source":"<name>",...}. The SQL `LIKE` is only a cheap
    // pre-filter: `source` is shared by every mount type (bind/config/secret),
    // so a `LIKE '%"source":"<name>"%'` match could be a bind or a secret with
    // the same name, not the named volume we're guarding. We confirm each
    // candidate by deserializing its `volumes` column and checking for a mount
    // that is actually `type == "volume"` with this source. Scoped to live
    // deployments — deleting a volume a stopped/failed deployment once used is
    // fine.
    let pattern = format!("%\"source\":\"{}\"%", volume_name);
    let sql = format!(
        "SELECT {} FROM deployment WHERE namespace = ? AND volumes LIKE ? AND status NOT IN ('deleted', 'completed', 'failed')",
        SELECT_COLUMNS
    );

    let rows = sqlx::query_as::<_, DeploymentRow>(&sql)
        .bind(namespace)
        .bind(&pattern)
        .fetch_all(pool)
        .await?;

    let referencing = rows
        .into_iter()
        .map(Deployment::from)
        .filter(|deployment| deployment_mounts_named_volume(&deployment.volumes, volume_name))
        .collect();

    Ok(referencing)
}

pub(crate) async fn delete_batch(
    pool: &SqlitePool,
    deleted: Vec<String>,
) -> Result<(), sqlx::Error> {
    for id in deleted {
        sqlx::query("DELETE FROM deployment WHERE id = ?")
            .bind(&id)
            .execute(pool)
            .await?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mounts_named_volume_matches_volume_type() {
        let json = r#"[{"type":"volume","source":"db-data","destination":"/data","driver":"local","permission":"rw"}]"#;
        assert!(deployment_mounts_named_volume(json, "db-data"));
    }

    #[test]
    fn mounts_named_volume_ignores_same_name_other_types() {
        // A bind, config or secret sharing the name must NOT count as a named
        // volume reference — a bare SQL LIKE on `source` would falsely match.
        let bind = r#"[{"type":"bind","source":"db-data","destination":"/data","driver":"local","permission":"rw"}]"#;
        let secret = r#"[{"type":"secret","source":"db-data","destination":"/run/secrets/db-data","driver":"local","permission":"ro"}]"#;
        assert!(!deployment_mounts_named_volume(bind, "db-data"));
        assert!(!deployment_mounts_named_volume(secret, "db-data"));
    }

    #[test]
    fn mounts_named_volume_handles_mixed_and_unparseable() {
        let mixed = r#"[{"type":"bind","source":"db-data","destination":"/x","driver":"local","permission":"rw"},{"type":"volume","source":"db-data","destination":"/data","driver":"local","permission":"rw"}]"#;
        assert!(deployment_mounts_named_volume(mixed, "db-data"));
        // Unparseable column can't be a structured volume mount → no reference.
        assert!(!deployment_mounts_named_volume("not json", "db-data"));
    }

    #[test]
    fn test_parse_memory_string_binary_suffixes() {
        assert_eq!(parse_memory_string("1Ki").unwrap(), 1024);
        assert_eq!(parse_memory_string("1Mi").unwrap(), 1024 * 1024);
        assert_eq!(parse_memory_string("512Mi").unwrap(), 512 * 1024 * 1024);
        assert_eq!(parse_memory_string("1Gi").unwrap(), 1024 * 1024 * 1024);
        assert_eq!(parse_memory_string("2Gi").unwrap(), 2 * 1024 * 1024 * 1024);
        assert_eq!(
            parse_memory_string("1Ti").unwrap(),
            1024i64 * 1024 * 1024 * 1024
        );
    }

    #[test]
    fn test_parse_memory_string_decimal_suffixes() {
        assert_eq!(parse_memory_string("1K").unwrap(), 1_000);
        assert_eq!(parse_memory_string("1M").unwrap(), 1_000_000);
        assert_eq!(parse_memory_string("1G").unwrap(), 1_000_000_000);
        assert_eq!(parse_memory_string("1T").unwrap(), 1_000_000_000_000);
    }

    #[test]
    fn test_parse_memory_string_raw_bytes() {
        assert_eq!(parse_memory_string("536870912").unwrap(), 536870912);
        assert_eq!(parse_memory_string("0").unwrap(), 0);
    }

    #[test]
    fn test_parse_memory_string_fractional() {
        assert_eq!(parse_memory_string("0.5Gi").unwrap(), 536870912);
        assert_eq!(
            parse_memory_string("1.5Mi").unwrap(),
            (1.5 * 1024.0 * 1024.0) as i64
        );
    }

    #[test]
    fn test_parse_memory_string_invalid() {
        assert!(parse_memory_string("abc").is_err());
        assert!(parse_memory_string("Mi").is_err());
        assert!(parse_memory_string("").is_err());
    }

    #[test]
    fn network_mode_default_is_bridge() {
        assert_eq!(NetworkMode::default(), NetworkMode::Bridge);
    }

    #[test]
    fn network_config_default_mode_is_bridge() {
        let cfg: NetworkConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(cfg.mode, NetworkMode::Bridge);
    }

    #[test]
    fn network_config_deserializes_host_mode() {
        let cfg: NetworkConfig = serde_json::from_str(r#"{"mode":"host"}"#).unwrap();
        assert_eq!(cfg.mode, NetworkMode::Host);
    }

    #[test]
    fn network_config_deserializes_bridge_mode() {
        let cfg: NetworkConfig = serde_json::from_str(r#"{"mode":"bridge"}"#).unwrap();
        assert_eq!(cfg.mode, NetworkMode::Bridge);
    }

    #[test]
    fn network_mode_rejects_unknown_value() {
        let parsed: Result<NetworkConfig, _> = serde_json::from_str(r#"{"mode":"macvlan"}"#);
        assert!(parsed.is_err());
    }

    #[test]
    fn network_mode_as_str_round_trips() {
        use std::str::FromStr;
        for mode in [NetworkMode::Bridge, NetworkMode::Host] {
            assert_eq!(NetworkMode::from_str(mode.as_str()).unwrap(), mode);
        }
    }
}
