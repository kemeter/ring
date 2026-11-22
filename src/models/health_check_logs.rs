use crate::models::health_check::{HealthCheckResult, HealthCheckStatus};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

#[derive(Serialize, Deserialize, Debug, Clone, sqlx::FromRow)]
pub(crate) struct HealthCheckResultRecord {
    pub(crate) id: String,
    pub(crate) deployment_id: String,
    pub(crate) instance_id: Option<String>,
    pub(crate) check_type: String,
    #[sqlx(default)]
    pub(crate) check_index: Option<i64>,
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
            instance_id: record.instance_id,
            check_type: record.check_type,
            check_index: record.check_index.and_then(|i| u32::try_from(i).ok()),
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
        "SELECT id, deployment_id, instance_id, check_type, status, message, created_at, started_at, finished_at
         FROM health_check WHERE deployment_id = ? ORDER BY started_at DESC LIMIT ?",
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
        "SELECT hcr.id, hcr.deployment_id, hcr.instance_id, hcr.check_type, hcr.status, hcr.message,
                hcr.created_at, hcr.started_at, hcr.finished_at
         FROM health_check hcr
         INNER JOIN (
             SELECT check_type, MAX(started_at) as max_started_at
             FROM health_check WHERE deployment_id = ?
             GROUP BY check_type
         ) latest ON hcr.check_type = latest.check_type AND hcr.started_at = latest.max_started_at
         WHERE hcr.deployment_id = ?
         ORDER BY hcr.check_type",
    )
    .bind(&deployment_id)
    .bind(&deployment_id)
    .fetch_all(pool)
    .await
}

/// Where one check stands on one instance, from its recent results.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CheckState {
    pub(crate) instance_id: String,
    pub(crate) check_index: u32,
    /// `success`, `failed` or `timeout`: the most recent result.
    pub(crate) latest_status: String,
    pub(crate) latest_finished_at: String,
    /// When the current run of successes began, i.e. the first success after
    /// the last non-success. `None` when the latest result is not a success.
    /// Anchoring the anti-flap window there, rather than on the latest
    /// success, lets it elapse instead of being re-armed by every probe.
    pub(crate) ready_since: Option<String>,
}

/// How many recent results are read to work out each check's state. Enough
/// for any realistic number of instances times checks; a run of successes
/// longer than this only starts its window later than it could, never earlier.
const RECENT_RESULTS: i64 = 5000;

/// The state of every check on every instance of `deployment_id`, one entry
/// per `(instance, check)` pair that has produced a result. Results stored
/// before checks were numbered are left out.
pub(crate) async fn find_check_states(
    pool: &SqlitePool,
    deployment_id: &str,
) -> Result<Vec<CheckState>, sqlx::Error> {
    let rows: Vec<(String, i64, String, String)> = sqlx::query_as(
        "SELECT instance_id, check_index, status, finished_at
         FROM health_check
         WHERE deployment_id = ? AND instance_id IS NOT NULL AND check_index IS NOT NULL
         ORDER BY finished_at DESC
         LIMIT ?",
    )
    .bind(deployment_id)
    .bind(RECENT_RESULTS)
    .fetch_all(pool)
    .await?;
    Ok(check_states(rows))
}

/// Fold results, newest first, into one state per `(instance, check)`.
fn check_states(rows: Vec<(String, i64, String, String)>) -> Vec<CheckState> {
    let mut states: Vec<CheckState> = Vec::new();
    // Pairs whose run of successes already met an older non-success.
    let mut closed: std::collections::HashSet<(String, u32)> = std::collections::HashSet::new();
    for (instance_id, index, status, finished_at) in rows {
        let Ok(check_index) = u32::try_from(index) else {
            continue;
        };
        let key = (instance_id.clone(), check_index);
        match states
            .iter_mut()
            .find(|s| s.instance_id == instance_id && s.check_index == check_index)
        {
            None => states.push(CheckState {
                instance_id,
                check_index,
                ready_since: (status == "success").then(|| finished_at.clone()),
                latest_status: status,
                latest_finished_at: finished_at,
            }),
            Some(state) => {
                if state.ready_since.is_none() || closed.contains(&key) {
                    continue;
                }
                if status == "success" {
                    state.ready_since = Some(finished_at);
                } else {
                    closed.insert(key);
                }
            }
        }
    }
    states
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

/// Forget the results of instances that are gone for good. Readiness is read
/// across the deployment, so a removed instance's old success would otherwise
/// count toward the anti-flap window of the instance replacing it.
pub(crate) async fn delete_by_instances(
    pool: &SqlitePool,
    deployment_id: &str,
    instance_ids: &[String],
) -> Result<u64, sqlx::Error> {
    if instance_ids.is_empty() {
        return Ok(0);
    }
    let placeholders = vec!["?"; instance_ids.len()].join(", ");
    let sql = format!(
        "DELETE FROM health_check WHERE deployment_id = ? AND instance_id IN ({placeholders})"
    );
    let mut query = sqlx::query(sqlx::AssertSqlSafe(sql)).bind(deployment_id);
    for id in instance_ids {
        query = query.bind(id);
    }
    Ok(query.execute(pool).await?.rows_affected())
}

pub(crate) async fn cleanup_old_health_checks(pool: &SqlitePool) -> Result<u64, sqlx::Error> {
    let deleted_by_age = sqlx::query(
        "DELETE FROM health_check WHERE datetime(started_at) < datetime('now', '-7 days')",
    )
    .execute(pool)
    .await?
    .rows_affected();

    let mut deleted_by_count: u64 = 0;

    let deployment_ids: Vec<String> =
        sqlx::query_scalar("SELECT DISTINCT deployment_id FROM health_check")
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
             )",
        )
        .bind(&deployment_id)
        .bind(&deployment_id)
        .execute(pool)
        .await?;

        deleted_by_count += result.rows_affected();
    }

    let total_deleted = deleted_by_age + deleted_by_count;
    if total_deleted > 0 {
        info!(
            "Cleaned up {} health check records ({} by age, {} by count limit)",
            total_deleted, deleted_by_age, deleted_by_count
        );
    }

    Ok(total_deleted)
}

#[cfg(test)]
mod check_state_tests {
    use super::check_states;

    fn row(instance: &str, index: i64, status: &str, at: &str) -> (String, i64, String, String) {
        (instance.into(), index, status.into(), at.into())
    }

    #[test]
    fn ready_since_is_the_start_of_the_current_run_of_successes() {
        // Newest first: ok@3, ok@2, failed@1, ok@0.
        let states = check_states(vec![
            row("i1", 0, "success", "3"),
            row("i1", 0, "success", "2"),
            row("i1", 0, "failed", "1"),
            row("i1", 0, "success", "0"),
        ]);
        assert_eq!(states.len(), 1);
        assert_eq!(states[0].latest_status, "success");
        assert_eq!(states[0].ready_since.as_deref(), Some("2"));
    }

    #[test]
    fn a_failing_check_is_not_ready() {
        let states = check_states(vec![
            row("i1", 0, "failed", "2"),
            row("i1", 0, "success", "1"),
        ]);
        assert_eq!(states[0].ready_since, None);
        assert_eq!(states[0].latest_status, "failed");
    }

    #[test]
    fn checks_and_instances_are_kept_apart() {
        // Two http checks (index 0 and 1) on two instances.
        let states = check_states(vec![
            row("i1", 0, "success", "4"),
            row("i1", 1, "failed", "4"),
            row("i2", 0, "success", "4"),
            row("i2", 1, "success", "4"),
        ]);
        assert_eq!(states.len(), 4);
        let failing: Vec<_> = states.iter().filter(|s| s.ready_since.is_none()).collect();
        assert_eq!(failing.len(), 1);
        assert_eq!(
            (failing[0].instance_id.as_str(), failing[0].check_index),
            ("i1", 1)
        );
    }
}
