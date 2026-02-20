use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::fmt;

pub(crate) const MAX_RESTART_COUNT: u32 = 5;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum DeploymentStatus {
    #[serde(rename = "pending")]
    Pending,
    #[serde(rename = "creating")]
    Creating,
    #[serde(rename = "running")]
    Running,
    #[serde(rename = "completed")]
    Completed,
    #[serde(rename = "failed")]
    Failed,
    #[serde(rename = "deleted")]
    Deleted,
    #[serde(rename = "CrashLoopBackOff")]
    CrashLoopBackOff,
    #[serde(rename = "ImagePullBackOff")]
    ImagePullBackOff,
    #[serde(rename = "CreateContainerError")]
    CreateContainerError,
    #[serde(rename = "NetworkError")]
    NetworkError,
    #[serde(rename = "ConfigError")]
    ConfigError,
    #[serde(rename = "FileSystemError")]
    FileSystemError,
    #[serde(rename = "Error")]
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
            Self::CrashLoopBackOff => write!(f, "CrashLoopBackOff"),
            Self::ImagePullBackOff => write!(f, "ImagePullBackOff"),
            Self::CreateContainerError => write!(f, "CreateContainerError"),
            Self::NetworkError => write!(f, "NetworkError"),
            Self::ConfigError => write!(f, "ConfigError"),
            Self::FileSystemError => write!(f, "FileSystemError"),
            Self::Error => write!(f, "Error"),
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
            "CrashLoopBackOff" => Ok(Self::CrashLoopBackOff),
            "ImagePullBackOff" => Ok(Self::ImagePullBackOff),
            "CreateContainerError" => Ok(Self::CreateContainerError),
            "NetworkError" => Ok(Self::NetworkError),
            "ConfigError" => Ok(Self::ConfigError),
            "FileSystemError" => Ok(Self::FileSystemError),
            "Error" => Ok(Self::Error),
            other => Err(format!("Unknown deployment status: {}", other)),
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
}

fn default_image_pull_policy() -> String {
    "Always".to_string()
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub(crate) struct ResourceLimits {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) cpu_limit: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) memory_limit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) memory_reservation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) cpu_shares: Option<i64>,
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
    pub(crate) secrets: HashMap<String, String>,
    pub(crate) volumes: String,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub(crate) health_checks: Vec<crate::models::health_check::HealthCheck>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub(crate) resources: Option<ResourceLimits>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    #[serde(skip_deserializing)]
    pub(crate) pending_events: Vec<crate::models::deployment_event::DeploymentEvent>,
}

impl Deployment {
    pub fn emit_event(&mut self, level: &str, message: String, component: &str, reason: Option<&str>) {
        let event = crate::models::deployment_event::DeploymentEvent::new(
            self.id.clone(),
            level,
            message,
            component,
            reason
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
    secrets: String,
    volumes: String,
    health_checks: Option<String>,
    resources: Option<String>,
}

impl From<DeploymentRow> for Deployment {
    fn from(row: DeploymentRow) -> Self {
        Deployment {
            id: row.id,
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
            command: serde_json::from_str(&row.command).unwrap_or_default(),
            instances: vec![],
            labels: serde_json::from_str(&row.labels).unwrap_or_default(),
            secrets: serde_json::from_str(&row.secrets).unwrap_or_default(),
            volumes: row.volumes,
            health_checks: row.health_checks
                .filter(|s| !s.is_empty())
                .map(|s| serde_json::from_str(&s).unwrap_or_default())
                .unwrap_or_default(),
            resources: row.resources
                .filter(|s| !s.is_empty())
                .and_then(|s| serde_json::from_str(&s).ok()),
            pending_events: vec![],
        }
    }
}

const SELECT_COLUMNS: &str = "
    id, created_at, updated_at, status, restart_count,
    namespace, name, image, command, config, runtime, kind,
    replicas, labels, secrets, volumes, health_checks, resources
";

pub(crate) async fn find_all(pool: &SqlitePool, filters: HashMap<String, Vec<String>>) -> Vec<Deployment> {
    let mut query = format!("SELECT {} FROM deployment", SELECT_COLUMNS);
    let mut all_values: Vec<String> = Vec::new();

    if !filters.is_empty() {
        let conditions: Vec<String> = filters
            .iter()
            .filter(|(_, v)| !v.is_empty())
            .map(|(column, values)| {
                let placeholders = values.iter().map(|_| "?").collect::<Vec<_>>().join(",");
                all_values.extend(values.clone());
                format!("{} IN({})", column, placeholders)
            })
            .collect();

        if !conditions.is_empty() {
            query += &format!(" WHERE {}", conditions.join(" AND "));
        }
    }

    let mut q = sqlx::query_as::<_, DeploymentRow>(&query);
    for val in &all_values {
        q = q.bind(val);
    }

    match q.fetch_all(pool).await {
        Ok(rows) => rows.into_iter().map(Deployment::from).collect(),
        Err(e) => {
            eprintln!("Could not execute SQL query: {}", e);
            Vec::new()
        }
    }
}

pub(crate) async fn find_active_by_namespace_name(
    pool: &SqlitePool,
    namespace: String,
    name: String,
) -> Result<Vec<Deployment>, sqlx::Error> {
    let sql = format!(
        "SELECT {} FROM deployment WHERE namespace = ? AND name = ? AND status <> 'deleted' ORDER BY created_at DESC",
        SELECT_COLUMNS
    );

    let rows = sqlx::query_as::<_, DeploymentRow>(&sql)
        .bind(&namespace)
        .bind(&name)
        .fetch_all(pool)
        .await?;

    Ok(rows.into_iter().map(Deployment::from).collect())
}

pub(crate) async fn find(pool: &SqlitePool, id: String) -> Result<Option<Deployment>, sqlx::Error> {
    let sql = format!("SELECT {} FROM deployment WHERE id = ?", SELECT_COLUMNS);

    let row = sqlx::query_as::<_, DeploymentRow>(&sql)
        .bind(&id)
        .fetch_optional(pool)
        .await?;

    Ok(row.map(Deployment::from))
}

pub(crate) async fn create(pool: &SqlitePool, deployment: &Deployment) -> Result<Deployment, sqlx::Error> {
    let labels = serde_json::to_string(&deployment.labels).unwrap_or_else(|_| "[]".to_string());
    let secrets = serde_json::to_string(&deployment.secrets).unwrap_or_else(|_| "[]".to_string());
    let config_json = match &deployment.config {
        Some(config) => serde_json::to_string(config).unwrap_or_else(|_| "{}".to_string()),
        None => "{}".to_string(),
    };
    let command_json = serde_json::to_string(&deployment.command).unwrap_or_else(|_| "[]".to_string());
    let health_checks_json = serde_json::to_string(&deployment.health_checks).unwrap_or_else(|_| "[]".to_string());

    let resources_json = deployment.resources.as_ref()
        .map(|r| serde_json::to_string(r).unwrap_or_else(|_| "null".to_string()));

    sqlx::query(
        "INSERT INTO deployment (
            id, created_at, status, restart_count, namespace, name, image,
            command, config, runtime, kind, replicas, labels, secrets, volumes, health_checks, resources
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
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
    .bind(&secrets)
    .bind(&deployment.volumes)
    .bind(&health_checks_json)
    .bind(&resources_json)
    .execute(pool)
    .await?;

    Ok(deployment.clone())
}

pub(crate) async fn update(pool: &SqlitePool, deployment: &Deployment) {
    let result = sqlx::query(
        "UPDATE deployment SET status = ?, updated_at = datetime('now'), restart_count = ? WHERE id = ?"
    )
    .bind(deployment.status.to_string())
    .bind(deployment.restart_count as i32)
    .bind(&deployment.id)
    .execute(pool)
    .await;

    if let Err(e) = result {
        eprintln!("Could not update deployment: {}", e);
    }
}

pub(crate) async fn delete_batch(pool: &SqlitePool, deleted: Vec<String>) {
    for id in deleted {
        let result = sqlx::query("DELETE FROM deployment WHERE id = ?")
            .bind(&id)
            .execute(pool)
            .await;

        if let Err(e) = result {
            eprintln!("Could not delete deployment: {}", e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_memory_string_binary_suffixes() {
        assert_eq!(parse_memory_string("1Ki").unwrap(), 1024);
        assert_eq!(parse_memory_string("1Mi").unwrap(), 1024 * 1024);
        assert_eq!(parse_memory_string("512Mi").unwrap(), 512 * 1024 * 1024);
        assert_eq!(parse_memory_string("1Gi").unwrap(), 1024 * 1024 * 1024);
        assert_eq!(parse_memory_string("2Gi").unwrap(), 2 * 1024 * 1024 * 1024);
        assert_eq!(parse_memory_string("1Ti").unwrap(), 1024i64 * 1024 * 1024 * 1024);
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
        assert_eq!(parse_memory_string("1.5Mi").unwrap(), (1.5 * 1024.0 * 1024.0) as i64);
    }

    #[test]
    fn test_parse_memory_string_invalid() {
        assert!(parse_memory_string("abc").is_err());
        assert!(parse_memory_string("Mi").is_err());
        assert!(parse_memory_string("").is_err());
    }
}
