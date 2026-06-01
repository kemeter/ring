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
    // Check-then-insert inside one transaction so two concurrent reconciles
    // can't both pass the existence check and race to insert. The unique index
    // on (namespace, name) is the final backstop: if a concurrent writer wins,
    // our insert hits a unique violation, which we treat as "already
    // registered" (Ok(false)) rather than a real error.
    let mut tx = pool.begin().await?;

    let exists = sqlx::query_as::<_, Volume>(&format!(
        "SELECT {COLUMNS} FROM volumes WHERE name = ? AND namespace = ?"
    ))
    .bind(name)
    .bind(namespace)
    .fetch_optional(&mut *tx)
    .await?
    .is_some();

    if exists {
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

    let insert_result = sqlx::query(
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
    .execute(&mut *tx)
    .await;

    match insert_result {
        Ok(_) => {
            tx.commit().await?;
            Ok(true)
        }
        // Use sqlx's typed constraint classification rather than matching on
        // the error message string, which varies across SQLite builds.
        Err(sqlx::Error::Database(db_err)) if db_err.is_unique_violation() => Ok(false),
        Err(e) => Err(e),
    }
}
