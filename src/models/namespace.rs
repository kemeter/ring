use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

#[derive(Serialize, Deserialize, Debug, Clone, sqlx::FromRow)]
pub(crate) struct Namespace {
    pub(crate) id: String,
    pub(crate) created_at: String,
    pub(crate) updated_at: Option<String>,
    pub(crate) name: String,
}

pub(crate) async fn find(pool: &SqlitePool, id: &str) -> Result<Option<Namespace>, sqlx::Error> {
    sqlx::query_as::<_, Namespace>(
        "SELECT id, created_at, updated_at, name FROM namespace WHERE id = ?",
    )
    .bind(&id)
    .fetch_optional(pool)
    .await
}

pub(crate) async fn find_by_name(
    pool: &SqlitePool,
    name: &str,
) -> Result<Option<Namespace>, sqlx::Error> {
    sqlx::query_as::<_, Namespace>(
        "SELECT id, created_at, updated_at, name FROM namespace WHERE name = ?",
    )
    .bind(name)
    .fetch_optional(pool)
    .await
}

pub(crate) async fn find_all(pool: &SqlitePool) -> Result<Vec<Namespace>, sqlx::Error> {
    sqlx::query_as::<_, Namespace>(
        "SELECT id, created_at, updated_at, name FROM namespace ORDER BY name",
    )
    .fetch_all(pool)
    .await
}

pub(crate) async fn create(pool: &SqlitePool, namespace: Namespace) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO namespace (id, created_at, updated_at, name) VALUES (?, ?, ?, ?)")
        .bind(&namespace.id)
        .bind(&namespace.created_at)
        .bind(&namespace.updated_at)
        .bind(&namespace.name)
        .execute(pool)
        .await?;

    Ok(())
}

pub(crate) async fn delete_by_name(pool: &SqlitePool, name: &str) -> Result<u64, sqlx::Error> {
    let result = sqlx::query("DELETE FROM namespace WHERE name = ?")
        .bind(name)
        .execute(pool)
        .await?;

    Ok(result.rows_affected())
}

/// Number of resources still living in a namespace. A namespace is only
/// deletable when this is zero — we never cascade-delete deployments,
/// secrets or configs out from under the operator. A "deleted"-status
/// deployment is a tombstone, not a live resource, so it does not count.
pub(crate) async fn count_resources(pool: &SqlitePool, name: &str) -> Result<i64, sqlx::Error> {
    let deployments: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM deployment WHERE namespace = ? AND status <> 'deleted'",
    )
    .bind(name)
    .fetch_one(pool)
    .await?;

    let secrets: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM secret WHERE namespace = ?")
        .bind(name)
        .fetch_one(pool)
        .await?;

    let configs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM config WHERE namespace = ?")
        .bind(name)
        .fetch_one(pool)
        .await?;

    Ok(deployments + secrets + configs)
}
