use rusqlite::{Connection, ToSql, Result, Row};
use rusqlite::named_params;
use serde::{Deserialize, Serialize};
use tokio::sync::MutexGuard;
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Volume {
    pub id: String,
    pub name: String,
    pub namespace: String,
    pub size: Option<u64>,
    pub backend_type: String,
    pub host_path: String,
    pub labels: HashMap<String, String>,
    pub created_at: String,
    pub updated_at: Option<String>,
}

impl Volume {
    pub(crate) fn create(
        name: String,
        namespace: String,
        size: Option<u64>,
        backend_type: String,
        host_path: String,
        labels: HashMap<String, String>,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name,
            namespace,
            size,
            backend_type,
            host_path,
            labels,
            created_at: chrono::Utc::now().to_rfc3339(),
            updated_at: None,
        }
    }

    pub(crate) fn insert(&self, connection: &MutexGuard<Connection>) -> Result<usize> {
        let labels_json = serde_json::to_string(&self.labels)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;

        connection.execute(
            "INSERT INTO volumes (id, name, namespace, size, backend_type, host_path, labels, created_at, updated_at)
             VALUES (:id, :name, :namespace, :size, :backend_type, :host_path, :labels, :created_at, :updated_at)",
            named_params! {
                ":id": self.id,
                ":name": self.name,
                ":namespace": self.namespace,
                ":size": self.size,
                ":backend_type": self.backend_type,
                ":host_path": self.host_path,
                ":labels": labels_json,
                ":created_at": self.created_at,
                ":updated_at": self.updated_at,
            },
        )
    }

    pub(crate) fn update(&mut self, connection: &MutexGuard<Connection>) -> Result<usize> {
        self.updated_at = Some(chrono::Utc::now().to_rfc3339());
        let labels_json = serde_json::to_string(&self.labels)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;

        connection.execute(
            "UPDATE volumes SET name = :name, namespace = :namespace, size = :size,
             backend_type = :backend_type, host_path = :host_path, labels = :labels, updated_at = :updated_at
             WHERE id = :id",
            named_params! {
                ":id": self.id,
                ":name": self.name,
                ":namespace": self.namespace,
                ":size": self.size,
                ":backend_type": self.backend_type,
                ":host_path": self.host_path,
                ":labels": labels_json,
                ":updated_at": self.updated_at,
            },
        )
    }

    pub(crate) fn get_by_name(name: &str, namespace: &str, connection: &MutexGuard<Connection>) -> Result<Option<Self>> {
        let mut stmt = connection.prepare(
            "SELECT id, name, namespace, size, backend_type, host_path, labels, created_at, updated_at
             FROM volumes WHERE name = :name AND namespace = :namespace"
        )?;

        let volume_iter = stmt.query_map(
            named_params! {
                ":name": name,
                ":namespace": namespace,
            },
            |row| Volume::from_row(row)
        )?;

        for volume in volume_iter {
            return Ok(Some(volume?));
        }

        Ok(None)
    }

    pub(crate) fn get_by_id(id: &str, connection: &MutexGuard<Connection>) -> Result<Option<Self>> {
        let mut stmt = connection.prepare(
            "SELECT id, name, namespace, size, backend_type, host_path, labels, created_at, updated_at
             FROM volumes WHERE id = :id"
        )?;

        let volume_iter = stmt.query_map(
            named_params! {
                ":id": id,
            },
            |row| Volume::from_row(row)
        )?;

        for volume in volume_iter {
            return Ok(Some(volume?));
        }

        Ok(None)
    }

    pub(crate) fn list_by_namespace(namespace: &str, connection: &MutexGuard<Connection>) -> Result<Vec<Self>> {
        let mut stmt = connection.prepare(
            "SELECT id, name, namespace, size, backend_type, host_path, labels, created_at, updated_at
             FROM volumes WHERE namespace = :namespace ORDER BY created_at DESC"
        )?;

        let volume_iter = stmt.query_map(
            named_params! {
                ":namespace": namespace,
            },
            |row| Volume::from_row(row)
        )?;

        let mut volumes = Vec::new();
        for volume in volume_iter {
            volumes.push(volume?);
        }

        Ok(volumes)
    }

    pub(crate) fn delete_by_name(name: &str, namespace: &str, connection: &MutexGuard<Connection>) -> Result<usize> {
        connection.execute(
            "DELETE FROM volumes WHERE name = :name AND namespace = :namespace",
            named_params! {
                ":name": name,
                ":namespace": namespace,
            },
        )
    }

    fn from_row(row: &Row) -> Result<Self> {
        let labels_json: String = row.get("labels")?;
        let labels: HashMap<String, String> = serde_json::from_str(&labels_json)
            .unwrap_or_else(|_| HashMap::new());

        Ok(Volume {
            id: row.get("id")?,
            name: row.get("name")?,
            namespace: row.get("namespace")?,
            size: row.get("size")?,
            backend_type: row.get("backend_type")?,
            host_path: row.get("host_path")?,
            labels,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}