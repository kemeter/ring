use rusqlite::{Connection, named_params};
use serde::{Deserialize, Serialize};
use tokio::sync::MutexGuard;
use crate::models::health_check::{HealthCheckResult, HealthCheckStatus};

#[derive(Serialize, Deserialize, Debug, Clone)]
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

pub(crate) fn find_by_deployment(
    connection: &MutexGuard<Connection>, 
    deployment_id: String,
    limit: Option<u32>
) -> Result<Vec<HealthCheckResultRecord>, rusqlite::Error> {
    let limit_clause = match limit {
        Some(l) => format!("LIMIT {}", l),
        None => "LIMIT 100".to_string(), // Default limit
    };

    let sql = format!("
        SELECT 
            id,
            deployment_id,
            check_type,
            status,
            message,
            created_at,
            started_at,
            finished_at
        FROM health_check 
        WHERE deployment_id = :deployment_id 
        ORDER BY started_at DESC
        {}
    ", limit_clause);

    let mut statement = connection.prepare(&sql)?;
    let result_iter = statement.query_map(
        named_params! {
            ":deployment_id": deployment_id,
        },
        |row| {
            Ok(HealthCheckResultRecord {
                id: row.get(0)?,
                deployment_id: row.get(1)?,
                check_type: row.get(2)?,
                status: row.get(3)?,
                message: row.get(4)?,
                created_at: row.get(5)?,
                started_at: row.get(6)?,
                finished_at: row.get(7)?,
            })
        },
    )?;

    let mut results = Vec::new();
    for result in result_iter {
        results.push(result?);
    }

    Ok(results)
}

pub(crate) fn find_latest_by_deployment(
    connection: &MutexGuard<Connection>, 
    deployment_id: String
) -> Result<Vec<HealthCheckResultRecord>, rusqlite::Error> {
    // Get the latest result for each check_type for a deployment
    let sql = "
        SELECT 
            hcr.id,
            hcr.deployment_id,
            hcr.check_type,
            hcr.status,
            hcr.message,
            hcr.created_at,
            hcr.started_at,
            hcr.finished_at
        FROM health_check hcr
        INNER JOIN (
            SELECT check_type, MAX(started_at) as max_started_at
            FROM health_check 
            WHERE deployment_id = :deployment_id
            GROUP BY check_type
        ) latest ON hcr.check_type = latest.check_type 
                  AND hcr.started_at = latest.max_started_at
        WHERE hcr.deployment_id = :deployment_id
        ORDER BY hcr.check_type
    ";

    let mut statement = connection.prepare(sql)?;
    let result_iter = statement.query_map(
        named_params! {
            ":deployment_id": deployment_id,
        },
        |row| {
            Ok(HealthCheckResultRecord {
                id: row.get(0)?,
                deployment_id: row.get(1)?,
                check_type: row.get(2)?,
                status: row.get(3)?,
                message: row.get(4)?,
                created_at: row.get(5)?,
                started_at: row.get(6)?,
                finished_at: row.get(7)?,
            })
        },
    )?;

    let mut results = Vec::new();
    for result in result_iter {
        results.push(result?);
    }

    Ok(results)
}

pub(crate) fn delete_by_deployment_id(
    connection: &MutexGuard<Connection>,
    deployment_id: &str
) -> Result<usize, rusqlite::Error> {
    let mut statement = connection.prepare("DELETE FROM health_check WHERE deployment_id = ?")?;
    let deleted_count = statement.execute(&[deployment_id])?;
    Ok(deleted_count)
}

pub(crate) fn cleanup_old_health_checks(
    connection: &MutexGuard<Connection>
) -> Result<usize, rusqlite::Error> {
    // First, delete health checks older than 7 days
    let mut statement = connection.prepare("DELETE FROM health_check WHERE datetime(started_at) < datetime('now', '-7 days')")?;
    let deleted_by_age = statement.execute([])?;
    
    // Then, for each deployment, keep only the 50 most recent health checks
    let mut deleted_by_count = 0;
    
    // Get all deployment IDs that have health checks
    let mut statement = connection.prepare("SELECT DISTINCT deployment_id FROM health_check")?;
    let deployment_ids: Vec<String> = statement.query_map([], |row| {
        Ok(row.get::<_, String>(0)?)
    })?.collect::<Result<Vec<_>, _>>()?;
    
    // For each deployment, delete old health checks beyond the 50 most recent
    for deployment_id in deployment_ids {
        let mut statement = connection.prepare(
            "DELETE FROM health_check 
             WHERE deployment_id = ? AND id NOT IN (
                 SELECT id FROM health_check 
                 WHERE deployment_id = ? 
                 ORDER BY datetime(started_at) DESC 
                 LIMIT 50
             )"
        )?;
        deleted_by_count += statement.execute(&[&deployment_id, &deployment_id])?;
    }
    
    let total_deleted = deleted_by_age + deleted_by_count;
    if total_deleted > 0 {
        info!("Cleaned up {} health check records ({} by age, {} by count limit)", 
              total_deleted, deleted_by_age, deleted_by_count);
    }
    
    Ok(total_deleted)
}

