use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

#[derive(Serialize, Deserialize, Debug, Clone, sqlx::FromRow)]
pub(crate) struct Namespace {
    pub(crate) id: String,
    pub(crate) created_at: String,
    pub(crate) updated_at: Option<String>,
    pub(crate) name: String,
}

pub(crate) async fn find(pool: &SqlitePool, id: String) -> Result<Option<Namespace>, sqlx::Error> {
    sqlx::query_as::<_, Namespace>("SELECT id, created_at, updated_at, name FROM namespace WHERE id = ?")
        .bind(&id)
        .fetch_optional(pool)
        .await
}

pub(crate) async fn find_by_name(pool: &SqlitePool, name: &str) -> Result<Option<Namespace>, sqlx::Error> {
    sqlx::query_as::<_, Namespace>("SELECT id, created_at, updated_at, name FROM namespace WHERE name = ?")
        .bind(name)
        .fetch_optional(pool)
        .await
}

pub(crate) async fn find_all(pool: &SqlitePool) -> Result<Vec<Namespace>, sqlx::Error> {
    sqlx::query_as::<_, Namespace>("SELECT id, created_at, updated_at, name FROM namespace ORDER BY name")
        .fetch_all(pool)
        .await
}

pub(crate) async fn create(pool: &SqlitePool, namespace: Namespace) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO namespace (id, created_at, updated_at, name) VALUES (?, ?, ?, ?)"
    )
    .bind(&namespace.id)
    .bind(&namespace.created_at)
    .bind(&namespace.updated_at)
    .bind(&namespace.name)
    .execute(pool)
    .await?;

    Ok(())
}
