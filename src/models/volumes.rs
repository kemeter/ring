// Ported from rusqlite to sqlx to complete the database-layer migration: this
// was the last module still importing `rusqlite`, which is no longer a
// dependency. The module has no callers yet (a first-class Volume entity is on
// the roadmap); `allow(dead_code)` keeps the faithful port in-tree and building
// without papering over real dead code elsewhere. The wiring that exercises
// these functions lands with the Volume entity feature, which removes this
// attribute.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub(crate) struct Volume {
    pub id: String,
    pub name: String,
    pub namespace: String,
    pub size: Option<i64>,
    pub backend_type: String,
    pub host_path: String,
    /// Stored as a JSON string in the column; use [`labels_map`] to decode.
    pub labels: String,
    pub created_at: String,
    pub updated_at: Option<String>,
}

impl Volume {
    pub(crate) fn create(
        name: String,
        namespace: String,
        size: Option<i64>,
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
            labels: serde_json::to_string(&labels).unwrap_or_else(|_| "{}".to_string()),
            created_at: chrono::Utc::now().to_rfc3339(),
            updated_at: None,
        }
    }

    pub(crate) fn labels_map(&self) -> HashMap<String, String> {
        serde_json::from_str(&self.labels).unwrap_or_default()
    }
}

const COLUMNS: &str =
    "id, name, namespace, size, backend_type, host_path, labels, created_at, updated_at";

pub(crate) async fn insert(pool: &SqlitePool, volume: &Volume) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO volumes (id, name, namespace, size, backend_type, host_path, labels, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&volume.id)
    .bind(&volume.name)
    .bind(&volume.namespace)
    .bind(volume.size)
    .bind(&volume.backend_type)
    .bind(&volume.host_path)
    .bind(&volume.labels)
    .bind(&volume.created_at)
    .bind(&volume.updated_at)
    .execute(pool)
    .await?;

    Ok(())
}

pub(crate) async fn update(pool: &SqlitePool, volume: &Volume) -> Result<(), sqlx::Error> {
    let updated_at = chrono::Utc::now().to_rfc3339();
    sqlx::query(
        "UPDATE volumes SET name = ?, namespace = ?, size = ?, backend_type = ?, \
         host_path = ?, labels = ?, updated_at = ? WHERE id = ?",
    )
    .bind(&volume.name)
    .bind(&volume.namespace)
    .bind(volume.size)
    .bind(&volume.backend_type)
    .bind(&volume.host_path)
    .bind(&volume.labels)
    .bind(&updated_at)
    .bind(&volume.id)
    .execute(pool)
    .await?;

    Ok(())
}

pub(crate) async fn get_by_name(
    pool: &SqlitePool,
    name: &str,
    namespace: &str,
) -> Result<Option<Volume>, sqlx::Error> {
    sqlx::query_as::<_, Volume>(&format!(
        "SELECT {COLUMNS} FROM volumes WHERE name = ? AND namespace = ?"
    ))
    .bind(name)
    .bind(namespace)
    .fetch_optional(pool)
    .await
}

pub(crate) async fn get_by_id(pool: &SqlitePool, id: &str) -> Result<Option<Volume>, sqlx::Error> {
    sqlx::query_as::<_, Volume>(&format!("SELECT {COLUMNS} FROM volumes WHERE id = ?"))
        .bind(id)
        .fetch_optional(pool)
        .await
}

pub(crate) async fn list_by_namespace(
    pool: &SqlitePool,
    namespace: &str,
) -> Result<Vec<Volume>, sqlx::Error> {
    sqlx::query_as::<_, Volume>(&format!(
        "SELECT {COLUMNS} FROM volumes WHERE namespace = ? ORDER BY created_at DESC"
    ))
    .bind(namespace)
    .fetch_all(pool)
    .await
}

pub(crate) async fn delete_by_name(
    pool: &SqlitePool,
    name: &str,
    namespace: &str,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query("DELETE FROM volumes WHERE name = ? AND namespace = ?")
        .bind(name)
        .bind(namespace)
        .execute(pool)
        .await?;

    Ok(result.rows_affected())
}
