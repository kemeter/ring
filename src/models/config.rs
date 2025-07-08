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
    let mut statement = connection.prepare("SELECT * FROM config").unwrap();
    let rows: Result<Vec<Config>, _> = from_rows::<Config>(statement.query([]).unwrap()).collect();

    let configs: Vec<Config> = rows.unwrap();

    configs
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
