use rusqlite::{Connection, ToSql, Result, Row};
use rusqlite::named_params;
use serde::{Deserialize, Serialize};
use tokio::sync::MutexGuard;
use std::collections::HashMap;
use std::fmt;
use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSqlOutput, Value as TypeValue, ValueRef};

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

impl ToSql for DeploymentStatus {
    fn to_sql(&self) -> Result<ToSqlOutput<'_>, rusqlite::Error> {
        Ok(ToSqlOutput::Owned(TypeValue::Text(self.to_string())))
    }
}

impl FromSql for DeploymentStatus {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        match value {
            ValueRef::Text(b) => {
                let s = std::str::from_utf8(b).map_err(|e| FromSqlError::Other(Box::new(e)))?;
                s.parse().map_err(|e: String| FromSqlError::Other(e.into()))
            }
            _ => Err(FromSqlError::InvalidType),
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

impl ToSql for DeploymentConfig {
    fn to_sql(&self) -> Result<ToSqlOutput<'_>, rusqlite::Error> {
        let json_string = serde_json::to_string(self)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        Ok(ToSqlOutput::Owned(TypeValue::Text(json_string)))
    }
}

impl FromSql for DeploymentConfig {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        match value {
            ValueRef::Blob(b) => {
                let s = std::str::from_utf8(b).map_err(|e| FromSqlError::Other(Box::new(e)))?;
                serde_json::from_str(s).map_err(|e| FromSqlError::Other(Box::new(e)))
            },
            ValueRef::Text(b) => {
                let s = std::str::from_utf8(b).map_err(|e| FromSqlError::Other(Box::new(e)))?;
                serde_json::from_str(s).map_err(|e| FromSqlError::Other(Box::new(e)))
            },
            _ => Err(FromSqlError::InvalidType),
        }
    }
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

    fn from_row(row: &Row) -> rusqlite::Result<Deployment> {
        Ok(Deployment {
            id: row.get("id")?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
            status: row.get::<_, DeploymentStatus>("status")?,
            restart_count: row.get("restart_count")?,
            namespace: row.get("namespace")?,
            name: row.get("name")?,
            image: row.get("image")?,
            config: match row.get::<_, Option<String>>("config") {
                Ok(Some(c)) => serde_json::from_str(&c).ok(),
                _ => None,
            },
            runtime: row.get("runtime")?,
            kind: row.get("kind")?,
            replicas: row.get("replicas")?,
            command: {
                let command_str: String = row.get("command")?;
                serde_json::from_str(&command_str).unwrap_or_default()
            },
            instances: vec![],
            labels: serde_json::from_str(&row.get::<_, String>("labels")?).unwrap_or_default(),
            secrets: serde_json::from_str(&row.get::<_, String>("secrets")?).unwrap_or_default(),
            volumes: row.get("volumes")?,
            health_checks: {
                let health_checks_str: String = row.get("health_checks").unwrap_or_default();
                if health_checks_str.is_empty() {
                    vec![]
                } else {
                    serde_json::from_str(&health_checks_str).unwrap_or_default()
                }
            },
            resources: {
                match row.get::<_, Option<String>>("resources") {
                    Ok(Some(s)) if !s.is_empty() => serde_json::from_str(&s).ok(),
                    _ => None,
                }
            },
            pending_events: vec![],
        })
    }
}

pub(crate) fn find_all(connection: &MutexGuard<Connection>, filters: HashMap<String, Vec<String>>) -> Vec<Deployment> {
    let mut query = String::from("
            SELECT
                id,
                created_at,
                updated_at,
                status,
                restart_count,
                namespace,
                name,
                image,
                command,
                config,
                runtime,
                kind,
                replicas,
                labels,
                secrets,
                volumes,
                health_checks,
                resources
            FROM deployment
    ");

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

    let values: Vec<&dyn rusqlite::ToSql> = all_values.iter().map(|v| v as &dyn rusqlite::ToSql).collect();

    let mut statement = match connection.prepare(&query) {
        Ok(stmt) => stmt,
        Err(e) => {
            eprintln!("Could not prepare SQL statement: {}", e);
            return Vec::new();
        }
    };

    let deployment_iter = match statement.query_map(&values[..], |row| {
        Deployment::from_row(row)
    }) {
        Ok(iter) => iter,
        Err(e) => {
            eprintln!("Could not execute SQL query: {}", e);
            return Vec::new();
        }
    };

    let mut deployments: Vec<Deployment> = Vec::new();
    for deployment in deployment_iter {
        match deployment {
            Ok(d) => deployments.push(d),
            Err(e) => eprintln!("Error processing row: {}", e),
        }
    }

    deployments
}

pub(crate) fn find_active_by_namespace_name(
    connection: &Connection,
    namespace: String,
    name: String
) -> Result<Vec<Deployment>, rusqlite::Error> {
    let sql = "
        SELECT
            id,
            created_at,
            updated_at,
            status,
            restart_count,
            namespace,
            name,
            image,
            command,
            runtime,
            kind,
            replicas,
            labels,
            secrets,
            volumes,
            health_checks,
            resources
        FROM deployment
        WHERE
            namespace = :namespace
            AND name = :name
            AND status <> 'deleted'
        ORDER BY created_at DESC
    ";

    let mut stmt = connection.prepare(sql)?;

    let deployment_iter = stmt.query_map(
        named_params! {
            ":namespace": namespace,
            ":name": name
        },
        Deployment::from_row,
    )?;

    let mut deployments = Vec::new();
    for deployment_result in deployment_iter {
        deployments.push(deployment_result?);
    }

    Ok(deployments)
}

pub(crate) fn find(connection: &MutexGuard<Connection>, id: String) -> Result<Option<Deployment>, serde_rusqlite::Error> {
    let mut statement = connection.prepare("
            SELECT
                id,
                created_at,
                updated_at,
                status,
                restart_count,
                namespace,
                name,
                image,
                command,
                config,
                runtime,
                kind,
                replicas,
                labels,
                secrets,
                volumes,
                health_checks,
                resources
            FROM deployment
            WHERE id = :id
            "
    ).expect("Could not fetch deployment");

    let mut deployments = statement.query_map(named_params!{":id": id}, |row| {
        Deployment::from_row(row)
    })?;

    if let Some(deployment) = deployments.next() {
        Ok(Some(deployment?))
    } else {
        Ok(None)
    }
}

pub(crate) fn create(connection: &MutexGuard<Connection>, deployment: &Deployment) -> Result<Deployment, rusqlite::Error> {

    let labels = serde_json::to_string(&deployment.labels).unwrap_or_else(|_| "[]".to_string());
    let secrets = serde_json::to_string(&deployment.secrets).unwrap_or_else(|_| "[]".to_string());

    let config_json = match &deployment.config {
        Some(config) => serde_json::to_string(config).unwrap_or_else(|_| "{}".to_string()),
        None => "{}".to_string(),
    };

    let mut statement = connection.prepare("
            INSERT INTO deployment (
                id,
                created_at,
                status,
                restart_count,
                namespace,
                name,
                image,
                command,
                config,
                runtime,
                kind,
                replicas,
                labels,
                secrets,
                volumes,
                health_checks,
                resources
            ) VALUES (
                :id,
                :created_at,
                :status,
                :restart_count,
                :namespace,
                :name,
                :image,
                :command,
                :config,
                :runtime,
                :kind,
                :replicas,
                :labels,
                :secrets,
                :volumes,
                :health_checks,
                :resources
            )"
    )?;

    let params = named_params!{
        ":id": deployment.id,
        ":created_at": deployment.created_at,
        ":status": deployment.status,
        ":restart_count": deployment.restart_count,
        ":namespace": deployment.namespace,
        ":name": deployment.name,
        ":image": deployment.image,
        ":command": serde_json::to_string(&deployment.command).unwrap_or_else(|_| "[]".to_string()),
        ":config": config_json,
        ":runtime": deployment.runtime,
        ":kind": deployment.kind,
        ":labels": labels,
        ":replicas": deployment.replicas,
        ":secrets": secrets,
        ":volumes": deployment.volumes,
        ":health_checks": serde_json::to_string(&deployment.health_checks).unwrap_or_else(|_| "[]".to_string()),
        ":resources": deployment.resources.as_ref().map(|r| serde_json::to_string(r).unwrap_or_else(|_| "null".to_string())),
    };

    statement.execute(params)?;

    Ok(deployment.clone())
}

pub(crate) fn update(connection: &MutexGuard<Connection>, deployment: &Deployment) {
    let mut statement = connection.prepare("
            UPDATE deployment
            SET
                status = :status,
                updated_at = datetime('now'),
                restart_count = :restart_count
            WHERE
                id = :id"
    ).expect("Could not update deployment");

    statement.execute(named_params!{
        ":id": deployment.id,
        ":status": deployment.status,
        ":restart_count": deployment.restart_count
    }).expect("Could not update deployment");
}

pub(crate) fn delete_batch(connection: &MutexGuard<Connection>, deleted: Vec<String>) {
    for id in deleted {
        let mut statement = connection.prepare("
            DELETE FROM deployment
            WHERE
                id = :id"
        ).expect("Could not delete deployment");

        statement.execute(named_params!{
            ":id": id
        }).expect("Could not delete deployment");
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
