use std::collections::HashMap;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

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

pub(crate) async fn find(pool: &SqlitePool, id: String) -> Result<Option<Config>, sqlx::Error> {
    sqlx::query_as::<_, Config>("SELECT id, created_at, updated_at, namespace, name, data, labels FROM config WHERE id = ?")
        .bind(&id)
        .fetch_optional(pool)
        .await
}

pub(crate) async fn find_all(pool: &SqlitePool, filters: HashMap<String, Vec<String>>) -> Vec<Config> {
    let mut query = String::from("SELECT id, created_at, updated_at, namespace, name, data, labels FROM config");
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

    let mut q = sqlx::query_as::<_, Config>(&query);
    for val in &all_values {
        q = q.bind(val);
    }

    match q.fetch_all(pool).await {
        Ok(configs) => configs,
        Err(e) => {
            log::error!("Failed to execute config query: {}", e);
            vec![]
        }
    }
}

pub(crate) async fn find_by_namespace(pool: &SqlitePool, namespace: String) -> Result<Vec<Config>, sqlx::Error> {
    sqlx::query_as::<_, Config>("SELECT id, created_at, updated_at, namespace, name, data, labels FROM config WHERE namespace = ?")
        .bind(&namespace)
        .fetch_all(pool)
        .await
}

pub(crate) async fn create(pool: &SqlitePool, config: Config) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO config (id, created_at, updated_at, namespace, name, data, labels) VALUES (?, ?, ?, ?, ?, ?, ?)"
    )
    .bind(&config.id)
    .bind(&config.created_at)
    .bind(&config.updated_at.unwrap_or_default())
    .bind(&config.namespace)
    .bind(&config.name)
    .bind(&config.data)
    .bind(&config.labels)
    .execute(pool)
    .await?;

    Ok(())
}

pub(crate) async fn delete(pool: &SqlitePool, id: String) -> Result<(), sqlx::Error> {
    let result = sqlx::query("DELETE FROM config WHERE id = ?")
        .bind(&id)
        .execute(pool)
        .await?;

    if result.rows_affected() == 0 {
        return Err(sqlx::Error::RowNotFound);
    }

    Ok(())
}

pub(crate) async fn update(pool: &SqlitePool, config: Config) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE config SET updated_at = ?, name = ?, data = ?, labels = ? WHERE id = ?")
        .bind(&config.updated_at.unwrap_or_default())
        .bind(&config.name)
        .bind(&config.data)
        .bind(&config.labels)
        .bind(&config.id)
        .execute(pool)
        .await?;

    Ok(())
}
