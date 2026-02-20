use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use uuid::Uuid;
use chrono::Utc;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct DeploymentEvent {
    pub id: String,
    pub deployment_id: String,
    pub timestamp: String,
    pub level: String,
    pub message: String,
    pub component: String,
    pub reason: Option<String>,
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
}

pub async fn create_event(pool: &SqlitePool, event: &DeploymentEvent) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO deployment_event (id, deployment_id, timestamp, level, message, component, reason)
         VALUES (?, ?, ?, ?, ?, ?, ?)"
    )
    .bind(&event.id)
    .bind(&event.deployment_id)
    .bind(&event.timestamp)
    .bind(&event.level)
    .bind(&event.message)
    .bind(&event.component)
    .bind(&event.reason)
    .execute(pool)
    .await?;

    sqlx::query("UPDATE deployment SET last_event_at = ? WHERE id = ?")
        .bind(&event.timestamp)
        .bind(&event.deployment_id)
        .execute(pool)
        .await?;

    Ok(())
}

pub async fn find_events_by_deployment(
    pool: &SqlitePool,
    deployment_id: &str,
    limit: Option<u32>,
) -> Result<Vec<DeploymentEvent>, sqlx::Error> {
    let safe_limit = limit.unwrap_or(100).min(1000) as i32;

    sqlx::query_as::<_, DeploymentEvent>(
        "SELECT id, deployment_id, timestamp, level, message, component, reason
         FROM deployment_event WHERE deployment_id = ? ORDER BY timestamp DESC LIMIT ?"
    )
    .bind(deployment_id)
    .bind(safe_limit)
    .fetch_all(pool)
    .await
}

pub async fn delete_by_deployment_id(pool: &SqlitePool, deployment_id: &str) -> Result<u64, sqlx::Error> {
    let result = sqlx::query("DELETE FROM deployment_event WHERE deployment_id = ?")
        .bind(deployment_id)
        .execute(pool)
        .await?;

    Ok(result.rows_affected())
}

pub async fn find_events_by_deployment_and_level(
    pool: &SqlitePool,
    deployment_id: &str,
    level: &str,
    limit: Option<u32>,
) -> Result<Vec<DeploymentEvent>, sqlx::Error> {
    let safe_limit = limit.unwrap_or(100).min(1000) as i32;

    sqlx::query_as::<_, DeploymentEvent>(
        "SELECT id, deployment_id, timestamp, level, message, component, reason
         FROM deployment_event WHERE deployment_id = ? AND level = ? ORDER BY timestamp DESC LIMIT ?"
    )
    .bind(deployment_id)
    .bind(level)
    .bind(safe_limit)
    .fetch_all(pool)
    .await
}

pub async fn log_event(
    pool: &SqlitePool,
    deployment_id: String,
    level: &str,
    message: String,
    component: &str,
    reason: Option<&str>,
) -> Result<(), sqlx::Error> {
    let event = DeploymentEvent::new(deployment_id, level, message, component, reason);
    create_event(pool, &event).await
}
