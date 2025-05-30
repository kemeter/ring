use std::collections::HashMap;
use chrono::{DateTime, Utc};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_rusqlite::{from_row, from_rows};
use tokio::sync::MutexGuard;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub(crate) struct Config {
    pub(crate) id: String,
    pub(crate) created_at: String,
    pub(crate) updated_at: Option<DateTime<Utc>>,
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
    statement.execute([id])?;
    Ok(())
}
