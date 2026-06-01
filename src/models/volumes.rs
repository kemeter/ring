//! First-class persistent volume entity.
//!
//! Gives a volume a lifecycle of its own — it can be pre-provisioned, listed,
//! and deleted independently of any deployment that mounts it — instead of
//! existing only as an inline entry in a deployment's `volumes JSON`. Backed by
//! the `volumes` table; `labels` is stored as a JSON string for parity with
//! `config`/`secret` (decode via [`Volume::labels_map`]).

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

/// Alias of [`get_by_id`] using the verb the API layer shares across resources
/// (`config::find`, `secret::find`).
pub(crate) async fn find(pool: &SqlitePool, id: &str) -> Result<Option<Volume>, sqlx::Error> {
    get_by_id(pool, id).await
}

const ALLOWED_FILTER_COLUMNS: &[&str] = &["namespace", "name"];

pub(crate) async fn find_all(
    pool: &SqlitePool,
    filters: HashMap<String, Vec<String>>,
) -> Result<Vec<Volume>, sqlx::Error> {
    let (query, values) = crate::models::query::build_filtered_query(
        &format!("SELECT {COLUMNS} FROM volumes"),
        &filters,
        ALLOWED_FILTER_COLUMNS,
    );

    let mut q = sqlx::query_as::<_, Volume>(&query);
    for val in &values {
        q = q.bind(val);
    }

    q.fetch_all(pool).await
}

pub(crate) async fn delete(pool: &SqlitePool, id: &str) -> Result<(), sqlx::Error> {
    let result = sqlx::query("DELETE FROM volumes WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;

    if result.rows_affected() == 0 {
        return Err(sqlx::Error::RowNotFound);
    }

    Ok(())
}

/// Record a volume in the registry if it isn't already there. This is the
/// retro-compatible path: a deployment may reference a named volume that was
/// never created through the `/volumes` API (the inline `volumes JSON` shape
/// that predates this entity). Rather than reject it, we register it on first
/// use so every volume becomes traceable, then leave it untouched if it already
/// exists. Returns `true` when a new row was inserted.
pub(crate) async fn register_if_absent(
    pool: &SqlitePool,
    namespace: &str,
    name: &str,
    backend_type: &str,
) -> Result<bool, sqlx::Error> {
    if get_by_name(pool, name, namespace).await?.is_some() {
        return Ok(false);
    }

    let volume = Volume::create(
        name.to_string(),
        namespace.to_string(),
        None,
        backend_type.to_string(),
        name.to_string(),
        HashMap::new(),
    );

    match insert(pool, &volume).await {
        Ok(_) => Ok(true),
        // A concurrent reconcile may have inserted the same (namespace, name)
        // between our check and insert — the unique index makes that a no-op,
        // not an error worth surfacing.
        Err(e) if e.to_string().contains("UNIQUE constraint failed") => Ok(false),
        Err(e) => Err(e),
    }
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
