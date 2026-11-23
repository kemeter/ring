use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use std::collections::HashMap;

#[derive(Serialize, Deserialize, Debug, Clone, sqlx::FromRow)]
pub(crate) struct Config {
    pub(crate) id: String,
    pub(crate) created_at: String,
    pub(crate) updated_at: Option<String>,
    pub(crate) namespace: String,
    pub(crate) name: String,
    pub(crate) data: String,
    pub(crate) labels: String,
}

pub(crate) async fn find(pool: &SqlitePool, id: &str) -> Result<Option<Config>, sqlx::Error> {
    sqlx::query_as::<_, Config>(
        "SELECT id, created_at, updated_at, namespace, name, data, labels FROM config WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

const ALLOWED_FILTER_COLUMNS: &[&str] = &["namespace", "name"];

pub(crate) async fn find_all(
    pool: &SqlitePool,
    filters: HashMap<String, Vec<String>>,
) -> Result<Vec<Config>, sqlx::Error> {
    let (query, values) = crate::models::query::build_filtered_query(
        "SELECT id, created_at, updated_at, namespace, name, data, labels FROM config",
        &filters,
        ALLOWED_FILTER_COLUMNS,
    );

    let mut q = sqlx::query_as::<_, Config>(sqlx::AssertSqlSafe(query));
    for val in &values {
        q = q.bind(val);
    }

    q.fetch_all(pool).await
}

pub(crate) async fn find_by_namespace(
    pool: &SqlitePool,
    namespace: &str,
) -> Result<Vec<Config>, sqlx::Error> {
    sqlx::query_as::<_, Config>("SELECT id, created_at, updated_at, namespace, name, data, labels FROM config WHERE namespace = ?")
        .bind(namespace)
        .fetch_all(pool)
        .await
}

pub(crate) async fn create(pool: &SqlitePool, config: Config) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO config (id, created_at, updated_at, namespace, name, data, labels) VALUES (?, ?, ?, ?, ?, ?, ?)"
    )
    .bind(&config.id)
    .bind(&config.created_at)
    .bind(config.updated_at.unwrap_or_default())
    .bind(&config.namespace)
    .bind(&config.name)
    .bind(&config.data)
    .bind(&config.labels)
    .execute(pool)
    .await?;

    Ok(())
}

pub(crate) async fn delete(pool: &SqlitePool, id: &str) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    let result = sqlx::query("DELETE FROM config WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;

    if result.rows_affected() == 0 {
        return Err(sqlx::Error::RowNotFound);
    }
    sqlx::query("DELETE FROM config_propagation WHERE config_id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    Ok(())
}

/// Store `config`. With `propagate`, also record that its change still has to
/// reach the deployments mounting it, leaving out those named there, in the
/// same transaction: a propagation cut short is then resumed, never lost.
pub(crate) async fn update(
    pool: &SqlitePool,
    config: Config,
    propagate: Option<&[String]>,
) -> Result<(), sqlx::Error> {
    let version = config.updated_at.unwrap_or_default();
    let mut tx = pool.begin().await?;
    sqlx::query("UPDATE config SET updated_at = ?, name = ?, data = ?, labels = ? WHERE id = ?")
        .bind(&version)
        .bind(&config.name)
        .bind(&config.data)
        .bind(&config.labels)
        .bind(&config.id)
        .execute(&mut *tx)
        .await?;
    if let Some(skip) = propagate {
        sqlx::query(
            "INSERT INTO config_propagation (config_id, version, skip_deployments) VALUES (?, ?, ?) \
             ON CONFLICT (config_id) DO UPDATE SET version = excluded.version, \
             skip_deployments = excluded.skip_deployments",
        )
        .bind(&config.id)
        .bind(&version)
        .bind(serde_json::to_string(skip).unwrap_or_else(|_| "[]".to_string()))
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;

    Ok(())
}

/// A config change that has not reached its deployments yet.
pub(crate) struct PendingPropagation {
    /// The config's `updated_at` the propagation was recorded for.
    pub(crate) version: String,
    /// Deployments the caller redeploys itself.
    pub(crate) skip: Vec<String>,
}

pub(crate) async fn pending_propagation(
    pool: &SqlitePool,
    config_id: &str,
) -> Result<Option<PendingPropagation>, sqlx::Error> {
    let row: Option<(String, String)> = sqlx::query_as(
        "SELECT version, skip_deployments FROM config_propagation WHERE config_id = ?",
    )
    .bind(config_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(version, skip)| PendingPropagation {
        version,
        skip: serde_json::from_str(&skip).unwrap_or_default(),
    }))
}

/// Ids of the configs whose change has not reached their deployments yet.
pub(crate) async fn pending_propagations(pool: &SqlitePool) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar("SELECT config_id FROM config_propagation ORDER BY config_id")
        .fetch_all(pool)
        .await
}

/// Mark the propagation of `version` done. A newer update recorded meanwhile
/// has another version, and stays pending.
pub(crate) async fn clear_propagation(
    pool: &SqlitePool,
    config_id: &str,
    version: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM config_propagation WHERE config_id = ? AND version = ?")
        .bind(config_id)
        .bind(version)
        .execute(pool)
        .await?;
    Ok(())
}
