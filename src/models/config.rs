use std::collections::HashMap;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_rusqlite::{from_row, from_rows};
use tokio::sync::MutexGuard;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub(crate) struct Config {
    pub(crate) id: String,
    pub(crate) created_at: String,
    pub(crate) updated_at: Option<String>,
    pub(crate) namespace: String,
    pub(crate) name: String,
    pub(crate) data: String,
    pub(crate) labels: String,
}

pub(crate) fn find(connection: &MutexGuard<Connection>, id: String) -> Result<Option<Config>, Box<dyn std::error::Error>> {
    let mut statement = connection.prepare("SELECT * FROM config WHERE id = ?1")?;
    let mut rows = statement.query([id])?;

    if let Some(row) = rows.next()? {
        let config = from_row::<Config>(&row)?;
        Ok(Some(config))
    } else {
        Ok(None)
    }
}

pub(crate) fn find_all(connection: &MutexGuard<Connection>, filters: HashMap<String, Vec<String>>) -> Vec<Config> {
    let mut query = String::from("
            SELECT
                id,
                created_at,
                updated_at,
                namespace,
                name,
                data,
                labels
            FROM config
    ");

    let mut all_values: Vec<&dyn rusqlite::ToSql> = Vec::new();
    
    if !filters.is_empty() {
        let conditions: Vec<String> = filters
            .iter()
            .filter(|(_, v)| !v.is_empty())
            .map(|(column, values)| {
                let placeholders = (0..values.len()).map(|_| "?").collect::<Vec<_>>().join(",");
                format!("{} IN({})", column, placeholders)
            })
            .collect();

        if !conditions.is_empty() {
            query += &format!(" WHERE {}", conditions.join(" AND "));
        }
        
        // Collect all values for parameter binding
        for (_, values) in filters.iter().filter(|(_, v)| !v.is_empty()) {
            for value in values {
                all_values.push(value as &dyn rusqlite::ToSql);
            }
        }
    }


    let mut statement = match connection.prepare(&query) {
        Ok(s) => s,
        Err(e) => {
            log::error!("Failed to prepare config query: {}", e);
            return vec![];
        }
    };

    let query_result = match statement.query(all_values.as_slice()) {
        Ok(r) => r,
        Err(e) => {
            log::error!("Failed to execute config query: {}", e);
            return vec![];
        }
    };

    let rows: Result<Vec<Config>, _> = from_rows::<Config>(query_result).collect();

    match rows {
        Ok(configs) => configs,
        Err(e) => {
            log::error!("Failed to parse config rows: {}", e);
            vec![]
        }
    }
}

pub(crate) fn delete(connection: &MutexGuard<Connection>, id: String) -> Result<(), Box<dyn std::error::Error>> {
    let mut statement = connection.prepare("DELETE FROM config WHERE id = ?1")?;
    let rows_affected = statement.execute([id])?;

    if rows_affected == 0 {
        return Err("Configuration not found".into());
    }

    Ok(())
}

pub(crate) fn find_by_namespace(connection: &MutexGuard<Connection>, namespace: String) -> Result<Vec<Config>, Box<dyn std::error::Error + Send + Sync>> {
    let mut statement = connection.prepare("SELECT * FROM config WHERE namespace = ?1")?;
    let rows = statement.query([namespace])?;
    let configs: Result<Vec<Config>, _> = from_rows::<Config>(rows).collect();
    configs.map_err(|e| e.into())
}

pub(crate) fn create(connection: &MutexGuard<Connection>, config: Config) -> Result<(), Box<dyn std::error::Error>> {
    let mut statement = connection.prepare(
        "INSERT INTO config (id, created_at, updated_at, namespace, name, data, labels)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)"
    )?;

    statement.execute([
        &config.id,
        &config.created_at,
        &config.updated_at.unwrap_or_default(),
        &config.namespace,
        &config.name,
        &config.data,
        &config.labels,
    ])?;

    Ok(())
}

pub(crate) fn update(connection: &MutexGuard<Connection>, config: Config) -> Result<(), Box<dyn std::error::Error>> {
    let mut statement = connection.prepare(
        "UPDATE config SET updated_at = ?1, name = ?2, data = ?3, labels = ?4 WHERE id = ?5"
    )?;

    statement.execute([
        &config.updated_at.unwrap_or_default(),
        &config.name,
        &config.data,
        &config.labels,
        &config.id,
    ])?;

    Ok(())
}
