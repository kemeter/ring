use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use uuid::Uuid;

/// Give up after this many failed delivery attempts and dead-letter the event.
pub(crate) const MAX_ATTEMPTS: i64 = 8;

/// A queued outbound event awaiting (or having completed) delivery.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub(crate) struct QueuedEvent {
    pub(crate) id: String,
    pub(crate) kind: String,
    /// JSON string of the event payload (parsed by the worker before sending).
    pub(crate) payload: String,
    pub(crate) status: String,
    pub(crate) attempts: i64,
    pub(crate) next_attempt_at: String,
    pub(crate) last_error: Option<String>,
    pub(crate) created_at: String,
    pub(crate) updated_at: Option<String>,
}

/// Exponential backoff for the Nth failed attempt: 1m, 2m, 4m, … capped at 1h.
/// `attempts` is the number of attempts already made (>= 1 when called).
pub(crate) fn next_backoff(attempts: i64) -> Duration {
    // 1,2,4,8,16,32 minutes for attempts 1..=6, then capped at 60. The shift
    // exponent is clamped to avoid overflow on large attempt counts.
    let exp = (attempts.max(1) - 1).min(10);
    let minutes = (1i64 << exp).min(60);
    Duration::minutes(minutes)
}

/// Enqueue an event for delivery. Starts `pending`, due immediately.
pub(crate) async fn enqueue(
    pool: &SqlitePool,
    kind: &str,
    payload: &str,
) -> Result<(), sqlx::Error> {
    let now = Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO events (id, kind, payload, status, attempts, next_attempt_at, created_at)
         VALUES (?, ?, ?, 'pending', 0, ?, ?)",
    )
    .bind(Uuid::new_v4().to_string())
    .bind(kind)
    .bind(payload)
    .bind(&now)
    .bind(&now)
    .execute(pool)
    .await?;

    Ok(())
}

/// Pending events whose `next_attempt_at` is due, oldest first, up to `limit`.
pub(crate) async fn claim_due(
    pool: &SqlitePool,
    limit: i64,
) -> Result<Vec<QueuedEvent>, sqlx::Error> {
    let now = Utc::now().to_rfc3339();
    sqlx::query_as::<_, QueuedEvent>(
        "SELECT id, kind, payload, status, attempts, next_attempt_at, last_error, created_at, updated_at
         FROM events
         WHERE status = 'pending' AND next_attempt_at <= ?
         ORDER BY next_attempt_at ASC
         LIMIT ?",
    )
    .bind(&now)
    .bind(limit)
    .fetch_all(pool)
    .await
}

pub(crate) async fn mark_delivered(pool: &SqlitePool, id: &str) -> Result<(), sqlx::Error> {
    let now = Utc::now().to_rfc3339();
    sqlx::query("UPDATE events SET status = 'delivered', updated_at = ? WHERE id = ?")
        .bind(&now)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Record a failed attempt and push `next_attempt_at` forward by the backoff.
/// `attempts` is the new attempt count (previous + 1).
pub(crate) async fn reschedule(
    pool: &SqlitePool,
    id: &str,
    attempts: i64,
    last_error: &str,
) -> Result<(), sqlx::Error> {
    let now = Utc::now();
    let next = (now + next_backoff(attempts)).to_rfc3339();
    sqlx::query(
        "UPDATE events SET attempts = ?, next_attempt_at = ?, last_error = ?, updated_at = ? WHERE id = ?",
    )
    .bind(attempts)
    .bind(&next)
    .bind(last_error)
    .bind(now.to_rfc3339())
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Terminal failure: stop retrying, keep the row for inspection.
pub(crate) async fn mark_dead(
    pool: &SqlitePool,
    id: &str,
    attempts: i64,
    last_error: &str,
) -> Result<(), sqlx::Error> {
    let now = Utc::now().to_rfc3339();
    sqlx::query(
        "UPDATE events SET status = 'dead', attempts = ?, last_error = ?, updated_at = ? WHERE id = ?",
    )
    .bind(attempts)
    .bind(last_error)
    .bind(&now)
    .bind(id)
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
        pool
    }

    #[tokio::test]
    async fn enqueue_then_claim_then_deliver() {
        let pool = test_pool().await;
        enqueue(&pool, "k", "{}").await.unwrap();
        let due = claim_due(&pool, 10).await.unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].attempts, 0);
        mark_delivered(&pool, &due[0].id).await.unwrap();
        assert!(claim_due(&pool, 10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn dead_lettered_event_is_never_claimed_again() {
        let pool = test_pool().await;
        enqueue(&pool, "k", "{}").await.unwrap();
        let id = claim_due(&pool, 10).await.unwrap()[0].id.clone();
        mark_dead(&pool, &id, MAX_ATTEMPTS, "boom").await.unwrap();
        // A dead event is terminal: it must not come back as due, ever.
        assert!(claim_due(&pool, 10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn reschedule_pushes_out_of_the_due_window() {
        let pool = test_pool().await;
        enqueue(&pool, "k", "{}").await.unwrap();
        let id = claim_due(&pool, 10).await.unwrap()[0].id.clone();
        reschedule(&pool, &id, 1, "transient").await.unwrap();
        // Backed off by >= 1 minute, so not due right now, but still pending.
        assert!(claim_due(&pool, 10).await.unwrap().is_empty());
    }

    #[test]
    fn backoff_grows_then_caps() {
        assert_eq!(next_backoff(1), Duration::minutes(1));
        assert_eq!(next_backoff(2), Duration::minutes(2));
        assert_eq!(next_backoff(3), Duration::minutes(4));
        assert_eq!(next_backoff(4), Duration::minutes(8));
        assert_eq!(next_backoff(5), Duration::minutes(16));
        assert_eq!(next_backoff(6), Duration::minutes(32));
        // Capped at 60 minutes beyond that.
        assert_eq!(next_backoff(7), Duration::minutes(60));
        assert_eq!(next_backoff(100), Duration::minutes(60));
    }
}
