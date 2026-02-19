use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use crate::models::health_check::{HealthCheckResult, HealthCheckStatus};

#[derive(Serialize, Deserialize, Debug, Clone, sqlx::FromRow)]
pub(crate) struct HealthCheckResultRecord {
    pub(crate) id: String,
    pub(crate) deployment_id: String,
    pub(crate) check_type: String,
    pub(crate) status: String,
    pub(crate) message: Option<String>,
    pub(crate) created_at: String,
    pub(crate) started_at: String,
    pub(crate) finished_at: String,
}

impl From<HealthCheckResultRecord> for HealthCheckResult {
    fn from(record: HealthCheckResultRecord) -> Self {
        let status = match record.status.as_str() {
            "success" => HealthCheckStatus::Success,
            "failed" => HealthCheckStatus::Failed,
            "timeout" => HealthCheckStatus::Timeout,
            _ => HealthCheckStatus::Failed,
        };

        HealthCheckResult {
            id: record.id,
            deployment_id: record.deployment_id,
            check_type: record.check_type,
            status,
            message: record.message,
            created_at: record.created_at,
            started_at: record.started_at,
            finished_at: record.finished_at,
        }
    }
}

pub(crate) async fn find_by_deployment(
    pool: &SqlitePool,
    deployment_id: String,
    limit: Option<u32>,
) -> Result<Vec<HealthCheckResultRecord>, sqlx::Error> {
    let limit_val = limit.unwrap_or(100) as i32;

    sqlx::query_as::<_, HealthCheckResultRecord>(
        "SELECT id, deployment_id, check_type, status, message, created_at, started_at, finished_at
         FROM health_check WHERE deployment_id = ? ORDER BY started_at DESC LIMIT ?"
    )
    .bind(&deployment_id)
    .bind(limit_val)
    .fetch_all(pool)
    .await
}

pub(crate) async fn find_latest_by_deployment(
    pool: &SqlitePool,
    deployment_id: String,
) -> Result<Vec<HealthCheckResultRecord>, sqlx::Error> {
    sqlx::query_as::<_, HealthCheckResultRecord>(
        "SELECT hcr.id, hcr.deployment_id, hcr.check_type, hcr.status, hcr.message,
                hcr.created_at, hcr.started_at, hcr.finished_at
         FROM health_check hcr
         INNER JOIN (
             SELECT check_type, MAX(started_at) as max_started_at
             FROM health_check WHERE deployment_id = ?
             GROUP BY check_type
         ) latest ON hcr.check_type = latest.check_type AND hcr.started_at = latest.max_started_at
         WHERE hcr.deployment_id = ?
         ORDER BY hcr.check_type"
    )
    .bind(&deployment_id)
    .bind(&deployment_id)
    .fetch_all(pool)
    .await
}

pub(crate) async fn delete_by_deployment_id(
    pool: &SqlitePool,
    deployment_id: &str,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query("DELETE FROM health_check WHERE deployment_id = ?")
        .bind(deployment_id)
        .execute(pool)
        .await?;

    Ok(result.rows_affected())
}

pub(crate) async fn cleanup_old_health_checks(
    pool: &SqlitePool,
) -> Result<u64, sqlx::Error> {
    let deleted_by_age = sqlx::query("DELETE FROM health_check WHERE datetime(started_at) < datetime('now', '-7 days')")
        .execute(pool)
        .await?
        .rows_affected();

    let mut deleted_by_count: u64 = 0;

    let deployment_ids: Vec<String> = sqlx::query_scalar("SELECT DISTINCT deployment_id FROM health_check")
        .fetch_all(pool)
        .await?;

    for deployment_id in deployment_ids {
        let result = sqlx::query(
            "DELETE FROM health_check
             WHERE deployment_id = ? AND id NOT IN (
                 SELECT id FROM health_check
                 WHERE deployment_id = ?
                 ORDER BY datetime(started_at) DESC
                 LIMIT 50
             )"
        )
        .bind(&deployment_id)
        .bind(&deployment_id)
        .execute(pool)
        .await?;

        deleted_by_count += result.rows_affected();
    }

    let total_deleted = deleted_by_age + deleted_by_count;
    if total_deleted > 0 {
        info!("Cleaned up {} health check records ({} by age, {} by count limit)",
              total_deleted, deleted_by_age, deleted_by_count);
    }

    Ok(total_deleted)
}
