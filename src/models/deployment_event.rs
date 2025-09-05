use rusqlite::{Connection, Result, Row};
use rusqlite::named_params;
use serde::{Deserialize, Serialize};
use tokio::sync::MutexGuard;
use uuid::Uuid;
use chrono::Utc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeploymentEvent {
    pub id: String,
    pub deployment_id: String,
    pub timestamp: String,
    pub level: String,        // "info", "warning", "error"
    pub message: String,
    pub component: String,    // "scheduler", "docker", "api"
    pub reason: Option<String>, // "ContainerStart", "ImagePull", "StateTransition", etc.
}

impl DeploymentEvent {
    pub fn new(
        deployment_id: String,
        level: &str,
        message: String,
        component: &str,
        reason: Option<&str>,
    ) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            deployment_id,
            timestamp: Utc::now().to_rfc3339(),
            level: level.to_string(),
            message,
            component: component.to_string(),
            reason: reason.map(|r| r.to_string()),
        }
    }

    fn from_row(row: &Row) -> Result<DeploymentEvent> {
        Ok(DeploymentEvent {
            id: row.get("id")?,
            deployment_id: row.get("deployment_id")?,
            timestamp: row.get("timestamp")?,
            level: row.get("level")?,
            message: row.get("message")?,
            component: row.get("component")?,
            reason: row.get("reason")?,
        })
    }
}

pub fn create_event(connection: &MutexGuard<Connection>, event: &DeploymentEvent) -> Result<()> {
    let mut statement = connection.prepare(
        "INSERT INTO deployment_event (id, deployment_id, timestamp, level, message, component, reason)
         VALUES (:id, :deployment_id, :timestamp, :level, :message, :component, :reason)"
    )?;

    statement.execute(named_params! {
        ":id": event.id,
        ":deployment_id": event.deployment_id,
        ":timestamp": event.timestamp,
        ":level": event.level,
        ":message": event.message,
        ":component": event.component,
        ":reason": event.reason,
    })?;

    // Update last_event_at in deployment table
    let mut update_statement = connection.prepare(
        "UPDATE deployment SET last_event_at = :timestamp WHERE id = :deployment_id"
    )?;

    update_statement.execute(named_params! {
        ":timestamp": event.timestamp,
        ":deployment_id": event.deployment_id,
    })?;

    Ok(())
}

pub fn find_events_by_deployment(
    connection: &MutexGuard<Connection>,
    deployment_id: &str,
    limit: Option<u32>
) -> Result<Vec<DeploymentEvent>> {
    let limit_clause = match limit {
        Some(l) => format!("LIMIT {}", l),
        None => String::new(),
    };

    let query = format!(
        "SELECT id, deployment_id, timestamp, level, message, component, reason
         FROM deployment_event
         WHERE deployment_id = :deployment_id
         ORDER BY timestamp DESC {}",
        limit_clause
    );

    let mut statement = connection.prepare(&query)?;
    
    let event_iter = statement.query_map(
        named_params! { ":deployment_id": deployment_id },
        DeploymentEvent::from_row
    )?;

    let mut events = Vec::new();
    for event in event_iter {
        events.push(event?);
    }

    Ok(events)
}

pub fn delete_by_deployment_id(connection: &MutexGuard<Connection>, deployment_id: &str) -> Result<usize> {
    let mut statement = connection.prepare("DELETE FROM deployment_event WHERE deployment_id = ?")?;
    let deleted_count = statement.execute(&[deployment_id])?;
    Ok(deleted_count)
}

pub fn find_events_by_deployment_and_level(
    connection: &MutexGuard<Connection>,
    deployment_id: &str,
    level: &str,
    limit: Option<u32>
) -> Result<Vec<DeploymentEvent>> {
    let limit_clause = match limit {
        Some(l) => format!("LIMIT {}", l),
        None => String::new(),
    };

    let query = format!(
        "SELECT id, deployment_id, timestamp, level, message, component, reason
         FROM deployment_event
         WHERE deployment_id = :deployment_id AND level = :level
         ORDER BY timestamp DESC {}",
        limit_clause
    );

    let mut statement = connection.prepare(&query)?;
    
    let event_iter = statement.query_map(
        named_params! { 
            ":deployment_id": deployment_id,
            ":level": level
        },
        DeploymentEvent::from_row
    )?;

    let mut events = Vec::new();
    for event in event_iter {
        events.push(event?);
    }

    Ok(events)
}

// Helper function to create and store an event in one call
pub fn log_event(
    connection: &MutexGuard<Connection>,
    deployment_id: String,
    level: &str,
    message: String,
    component: &str,
    reason: Option<&str>,
) -> Result<()> {
    let event = DeploymentEvent::new(deployment_id, level, message, component, reason);
    create_event(connection, &event)
}