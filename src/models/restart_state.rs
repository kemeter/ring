//! Persisted restart state of a deployment: when it may be started again, how
//! long its instances have been running, and how the last one ended.
//!
//! Kept apart from [`crate::models::deployments::Deployment`] and written with
//! targeted statements, like `restart_count`: the scheduler's full-row write
//! back must never carry these fields, or a stale copy would undo a backoff.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use std::collections::HashMap;

/// How an instance ended, kept after the instance itself is removed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Termination {
    pub(crate) instance_id: String,
    /// `None` when the runtime could not tell.
    pub(crate) exit_code: Option<i64>,
    pub(crate) finished_at: DateTime<Utc>,
    /// Last lines of the instance's output, at most [`LOGS_TAIL_BYTES`].
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub(crate) logs_tail: Option<String>,
}

/// Lines of output kept from a terminated instance.
pub(crate) const LOGS_TAIL_LINES: usize = 80;
/// Upper bound on the output kept from a terminated instance.
pub(crate) const LOGS_TAIL_BYTES: usize = 2048;

/// Keep the end of `lines`: at most [`LOGS_TAIL_LINES`] lines and
/// [`LOGS_TAIL_BYTES`] bytes, whichever is smaller. `None` when there is
/// nothing to keep.
pub(crate) fn logs_tail(lines: &[String]) -> Option<String> {
    let start = lines.len().saturating_sub(LOGS_TAIL_LINES);
    let joined = lines[start..].join("\n");
    if joined.is_empty() {
        return None;
    }
    if joined.len() <= LOGS_TAIL_BYTES {
        return Some(joined);
    }
    let mut cut = joined.len() - LOGS_TAIL_BYTES;
    while !joined.is_char_boundary(cut) {
        cut += 1;
    }
    Some(joined[cut..].to_string())
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RestartState {
    pub(crate) next_attempt_at: Option<DateTime<Utc>>,
    pub(crate) running_since: Option<DateTime<Utc>>,
    pub(crate) last_termination: Option<Termination>,
    /// Runs of a job that exited non-zero.
    pub(crate) run_failures: u32,
}

impl RestartState {
    /// Whether the deployment must not be started before a later tick.
    pub(crate) fn backing_off(&self, now: DateTime<Utc>) -> bool {
        self.next_attempt_at.is_some_and(|at| at > now)
    }
}

#[derive(sqlx::FromRow)]
struct Row {
    id: String,
    next_attempt_at: Option<String>,
    running_since: Option<String>,
    last_termination: Option<String>,
    run_failures: i64,
}

fn parse_time(raw: Option<String>) -> Option<DateTime<Utc>> {
    raw.and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
        .map(|t| t.with_timezone(&Utc))
}

/// The restart state of every deployment that has one, by deployment id.
/// Deployments with nothing recorded are left out.
pub(crate) async fn find_all(
    pool: &SqlitePool,
) -> Result<HashMap<String, RestartState>, sqlx::Error> {
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT id, next_attempt_at, running_since, last_termination, run_failures FROM deployment \
         WHERE next_attempt_at IS NOT NULL OR running_since IS NOT NULL \
            OR last_termination IS NOT NULL OR run_failures > 0",
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| {
            let state = RestartState {
                next_attempt_at: parse_time(row.next_attempt_at),
                running_since: parse_time(row.running_since),
                last_termination: row
                    .last_termination
                    .and_then(|json| serde_json::from_str(&json).ok()),
                run_failures: u32::try_from(row.run_failures).unwrap_or(0),
            };
            (row.id, state)
        })
        .collect())
}

/// Store the restart state of one deployment.
pub(crate) async fn save(
    pool: &SqlitePool,
    deployment_id: &str,
    state: &RestartState,
) -> Result<(), sqlx::Error> {
    let last_termination = match &state.last_termination {
        Some(t) => Some(serde_json::to_string(t).map_err(|e| sqlx::Error::Encode(Box::new(e)))?),
        None => None,
    };
    sqlx::query(
        "UPDATE deployment SET next_attempt_at = ?, running_since = ?, last_termination = ?, \
         run_failures = ? WHERE id = ?",
    )
    .bind(state.next_attempt_at.map(|t| t.to_rfc3339()))
    .bind(state.running_since.map(|t| t.to_rfc3339()))
    .bind(last_termination)
    .bind(i64::from(state.run_failures))
    .bind(deployment_id)
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn test_pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO deployment (id, created_at, status, namespace, runtime, kind, name, restart_count) \
             VALUES ('d1', '2026-01-01', 'running', 'ns', 'docker', 'worker', 'w', 0), \
                    ('d2', '2026-01-01', 'running', 'ns', 'docker', 'worker', 'w2', 0)",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[tokio::test]
    async fn saved_state_is_read_back() {
        let pool = test_pool().await;
        let state = RestartState {
            next_attempt_at: Some(at("2026-01-01T00:05:00Z")),
            running_since: None,
            last_termination: Some(Termination {
                instance_id: "c1".to_string(),
                exit_code: Some(1),
                finished_at: at("2026-01-01T00:00:00Z"),
                logs_tail: Some("boom".to_string()),
            }),
            run_failures: 2,
        };
        save(&pool, "d1", &state).await.unwrap();

        let all = find_all(&pool).await.unwrap();
        assert_eq!(all.get("d1"), Some(&state));
        assert!(
            !all.contains_key("d2"),
            "nothing recorded, nothing returned"
        );
    }

    #[tokio::test]
    async fn saving_an_empty_state_clears_it() {
        let pool = test_pool().await;
        let state = RestartState {
            next_attempt_at: Some(at("2026-01-01T00:05:00Z")),
            ..Default::default()
        };
        save(&pool, "d1", &state).await.unwrap();
        save(&pool, "d1", &RestartState::default()).await.unwrap();
        assert!(find_all(&pool).await.unwrap().is_empty());
    }

    #[test]
    fn backing_off_only_until_the_next_attempt() {
        let state = RestartState {
            next_attempt_at: Some(at("2026-01-01T00:05:00Z")),
            ..Default::default()
        };
        assert!(state.backing_off(at("2026-01-01T00:04:59Z")));
        assert!(!state.backing_off(at("2026-01-01T00:05:00Z")));
        assert!(!RestartState::default().backing_off(at("2026-01-01T00:00:00Z")));
    }

    #[test]
    fn logs_tail_keeps_the_last_lines() {
        let lines: Vec<String> = (0..100).map(|i| format!("l{i}")).collect();
        let tail = logs_tail(&lines).unwrap();
        assert!(tail.starts_with("l20\n"));
        assert!(tail.ends_with("l99"));
    }

    #[test]
    fn logs_tail_is_bounded_in_bytes() {
        let lines = vec!["é".repeat(3000)];
        let tail = logs_tail(&lines).unwrap();
        assert!(tail.len() <= LOGS_TAIL_BYTES);
        assert!(tail.chars().all(|c| c == 'é'));
    }

    #[test]
    fn logs_tail_of_nothing_is_none() {
        assert_eq!(logs_tail(&[]), None);
    }
}
