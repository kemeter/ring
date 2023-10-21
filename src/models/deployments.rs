use rusqlite::{Connection, ToSql, Result, Row};
use rusqlite::named_params;
use serde::{Deserialize, Serialize};
use serde_rusqlite::from_rows;
use serde_rusqlite::from_rows_ref;
use tokio::sync::MutexGuard;
use std::collections::HashMap;
use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSqlOutput, Value as TypeValue, ValueRef};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub(crate) struct DeploymentConfig {
    #[serde(default = "default_image_pull_policy")]
    pub(crate) image_pull_policy: String,
    pub(crate) server: Option<String>,
    pub(crate) username: Option<String>,
    pub(crate) password: Option<String>,
}

fn default_image_pull_policy() -> String {
    "Always".to_string()
}

impl ToSql for DeploymentConfig {
    fn to_sql(&self) -> Result<ToSqlOutput<'_>, rusqlite::Error> {
        let json_string = serde_json::to_string(self).unwrap();
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
    pub(crate) status: String,
    pub(crate) namespace: String,
    pub(crate) name: String,
    pub(crate) image: String,
    pub(crate) config: Option<DeploymentConfig>,
    pub(crate) runtime: String,
    pub(crate) kind: String,
    pub(crate) replicas: u32,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub(crate) instances: Vec<String>,
    #[serde(skip_deserializing)]
    pub(crate) labels: HashMap<String, String>,
    #[serde(skip_deserializing)]
    pub(crate) secrets: HashMap<String, String>,
    pub(crate) volumes: String,
}

impl Deployment {
    fn from_row(row: &Row) -> rusqlite::Result<Deployment> {
        Ok(Deployment {
            id: row.get("id")?,
            created_at: row.get("created_at")?,
            status: row.get("status")?,
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
            instances: vec![],
            labels: serde_json::from_str(&row.get::<_, String>("labels")?).unwrap_or_default(),
            secrets: serde_json::from_str(&row.get::<_, String>("secrets")?).unwrap_or_default(),
            volumes: row.get("volumes")?
        })
    }
}

pub(crate) fn find_all(connection: &MutexGuard<Connection>, filters: HashMap<String, String>) -> Vec<Deployment> {
    let mut query = String::from("
            SELECT
                id,
                created_at,
                status,
                namespace,
                name,
                image,
                config,
                runtime,
                kind,
                replicas,
                labels,
                secrets,
                volumes
            FROM deployment
    ");

    if !filters.is_empty() {
        let conditions: Vec<String> = filters
            .keys()
            .map(|column| format!("{} = ?", column))
            .collect();
        query.push_str(" WHERE ");
        query.push_str(&conditions.join(" AND "));
    }

    let values: Vec<&dyn rusqlite::ToSql> = filters.values().map(|v| v as &dyn rusqlite::ToSql).collect();
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


pub(crate) fn find_one_by_filters(connection: &Connection, filters: Vec<String>) -> Result<Option<Deployment>, rusqlite::Error> {

    debug!("find_one_by_filters {:?}", filters);

    let mut statement = connection.prepare("
            SELECT
                id,
                created_at,
                status,
                namespace,
                name,
                image,
                runtime,
                kind,
                replicas,
                labels,
                labels as labelsjson,
                secrets,
                secrets as secretsjson,
                volumes
            FROM deployment
            WHERE
                namespace = :namespace
                AND name = :name
                AND status = :status
            "
    ).expect("Could not fetch deployment");

    let mut rows = statement.query_map(named_params!{
        ":namespace": filters.get(0).unwrap_or(&String::from("")),
        ":name": filters.get(1).unwrap_or(&String::from("")),
        ":status": "running"
    }, |row| {
        Deployment::from_row(row)
    })?;

    match rows.next() {
        Some(Ok(deployment)) => Ok(Some(deployment)),
        Some(Err(e)) => Err(e),
        None => Ok(None),
    }
}

pub(crate) fn find(connection: &MutexGuard<Connection>, id: String) -> Result<Option<Deployment>, serde_rusqlite::Error> {
    let mut statement = connection.prepare("
            SELECT
                id,
                created_at,
                status,
                namespace,
                name,
                image,
                config,
                runtime,
                kind,
                replicas,
                labels,
                labels as labelsjson,
                secrets,
                secrets as secretsjson,
                volumes
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

pub(crate) fn create(connection: &MutexGuard<Connection>, deployment: &Deployment) -> Deployment {

    let labels = serde_json::to_string(&deployment.labels).unwrap();
    let secrets = serde_json::to_string(&deployment.secrets).unwrap();

    let config_json = match &deployment.config {
        Some(config) => serde_json::to_string(config).unwrap_or_else(|_| "{}".to_string()),
        None => "{}".to_string(),
    };

    let mut statement = connection.prepare("
            INSERT INTO deployment (
                id,
                created_at,
                status,
                namespace,
                name,
                image,
                config,
                runtime,
                kind,
                replicas,
                labels,
                secrets,
                volumes
            ) VALUES (
                :id,
                :created_at,
                :status,
                :namespace,
                :name,
                :image,
                :config,
                :runtime,
                :kind,
                :replicas,
                :labels,
                :secrets,
                :volumes
            )"
    ).expect("Could not create deployment");

    let params = named_params!{
        ":id": deployment.id,
        ":created_at": deployment.created_at,
        ":status": "running",
        ":namespace": deployment.namespace,
        ":name": deployment.name,
        ":image": deployment.image,
        ":config": config_json,
        ":runtime": deployment.runtime,
        ":kind": deployment.kind,
        ":labels": labels,
        ":replicas": deployment.replicas,
        ":secrets": secrets,
        ":volumes": deployment.volumes,
    };

    statement.execute(params).expect("Could not create deployment");

    return deployment.clone();
}

pub(crate) fn update(connection: &MutexGuard<Connection>, deployment: &Deployment) {
    let mut statement = connection.prepare("
            UPDATE deployment
            SET
                status = :status
            WHERE
                id = :id"
    ).expect("Could not update deployment");

    statement.execute(named_params!{
        ":id": deployment.id,
        ":status": deployment.status
    }).expect("Could not update deployment");
}


pub(crate) fn delete(connection: &MutexGuard<Connection>, id: String) {
    let mut statement = connection.prepare("
            DELETE FROM deployment
            WHERE
                id = :id"
    ).expect("Could not update deployment");

    statement.execute(named_params!{
        ":id": id
    }).expect("Could not delete deployment");
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




