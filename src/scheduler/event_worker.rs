//! Outbound event delivery worker.
//!
//! A standalone tokio task that drains the `events` queue and delivers each due
//! event to every webhook subscribed to its kind, with exponential backoff and
//! a dead-letter terminal state. Runs independently of the reconciliation
//! scheduler so delivery latency never stalls a scheduling tick.

use crate::models::event_queue::{self, MAX_ATTEMPTS, QueuedEvent};
use crate::models::webhook;
use crate::webhook as delivery;
use sqlx::SqlitePool;
use std::time::Duration;
use tokio::time::{sleep, timeout};

/// How many due events to pull per tick.
const BATCH: i64 = 50;

/// Per-request delivery timeout (mirrors the one set in `webhook::deliver`).
const DELIVERY_TIMEOUT_SECS: u64 = 10;

/// Hard ceiling on one tick. Events are processed in order, so a long run of
/// failing-slow subscribers could otherwise hold the loop for `BATCH ×
/// DELIVERY_TIMEOUT` before the next poll. Bound it so the worker stays
/// responsive: anything not delivered this tick is still pending and picked up
/// on the next one.
const TICK_BUDGET: Duration = Duration::from_secs(BATCH as u64 * DELIVERY_TIMEOUT_SECS);

/// Run the worker loop forever. `interval_secs` is how often the queue is
/// polled for due events.
pub(crate) async fn run(pool: SqlitePool, interval_secs: u64) {
    let tick = Duration::from_secs(interval_secs.max(1));
    loop {
        match timeout(TICK_BUDGET, process_due(&pool)).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => warn!("Event worker tick failed: {}", e),
            Err(_) => warn!(
                "Event worker tick exceeded {}s budget; resuming next tick",
                TICK_BUDGET.as_secs()
            ),
        }
        sleep(tick).await;
    }
}

async fn process_due(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    let due = event_queue::fetch_due(pool, BATCH).await?;
    for event in due {
        deliver_event(pool, &event).await;
    }
    Ok(())
}

/// Deliver one queued event to all matching subscribers. The event is
/// `delivered` only if every subscriber accepted it; any failure reschedules
/// the whole event (or dead-letters it past MAX_ATTEMPTS). Redelivery to
/// already-succeeded subscribers on retry is acceptable — webhooks are
/// at-least-once, receivers must be idempotent.
async fn deliver_event(pool: &SqlitePool, event: &QueuedEvent) {
    let subscribers = match webhook::subscribers_for(pool, &event.kind).await {
        Ok(s) => s,
        Err(e) => {
            warn!("Failed to load subscribers for {}: {}", event.kind, e);
            return; // leave pending; retried next tick without bumping attempts
        }
    };

    // No subscriber for this kind: nothing to do, mark delivered so it doesn't
    // sit pending forever.
    if subscribers.is_empty() {
        let _ = event_queue::mark_delivered(pool, &event.id).await;
        return;
    }

    // Deliver to every subscriber concurrently: they are independent endpoints,
    // and a single slow one must not serialise the rest behind its 10s timeout.
    // Without this, one hung subscriber blocks delivery of this event to all
    // others (and, since events are processed in order, the whole tick).
    let body = event.payload.as_bytes();
    let results = futures::future::join_all(
        subscribers
            .iter()
            .map(|hook| delivery::deliver(hook, &event.kind, body)),
    )
    .await;

    let mut first_error: Option<String> = None;
    for (hook, result) in subscribers.iter().zip(results) {
        if let Err(e) = result {
            warn!(
                "Webhook {} delivery to {} failed: {}",
                event.kind, hook.url, e
            );
            first_error.get_or_insert(e);
        }
    }

    match first_error {
        None => {
            let _ = event_queue::mark_delivered(pool, &event.id).await;
        }
        Some(err) => {
            let attempts = event.attempts + 1;
            if attempts >= MAX_ATTEMPTS {
                let _ = event_queue::mark_dead(pool, &event.id, attempts, &err).await;
                warn!(
                    "Event {} ({}) dead-lettered after {} attempts: {}",
                    event.id, event.kind, attempts, err
                );
            } else {
                let _ = event_queue::reschedule(pool, &event.id, attempts, &err).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// Minimal HTTP/1.1 mock: accepts connections, records the number of
    /// requests and whether the last carried an X-Ring-Signature, and replies
    /// with `status_for(n)` where n is the 1-based request index. Lets a test
    /// drive "fail twice then succeed" deterministically without wiremock.
    struct Mock {
        url: String,
        hits: Arc<AtomicUsize>,
        signed: Arc<AtomicUsize>,
    }

    async fn start_mock(status_for: impl Fn(usize) -> u16 + Send + 'static) -> Mock {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let signed = Arc::new(AtomicUsize::new(0));
        let (h, s) = (hits.clone(), signed.clone());
        tokio::spawn(async move {
            loop {
                let (mut sock, _) = match listener.accept().await {
                    Ok(p) => p,
                    Err(_) => break,
                };
                // Read the request (headers + small body) — enough to inspect
                // headers; we don't need to fully drain large bodies here.
                let mut buf = vec![0u8; 4096];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_lowercase();
                let idx = h.fetch_add(1, Ordering::SeqCst) + 1;
                if req.contains("x-ring-signature:") {
                    s.fetch_add(1, Ordering::SeqCst);
                }
                let code = status_for(idx);
                let body = "ok";
                let resp = format!(
                    "HTTP/1.1 {} X\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    code,
                    body.len(),
                    body
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.flush().await;
            }
        });
        Mock {
            url: format!("http://{}/hook", addr),
            hits,
            signed,
        }
    }

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
    async fn delivers_signed_payload_then_marks_delivered() {
        let pool = test_pool().await;
        let mock = start_mock(|_| 200).await;
        webhook::create(&pool, &mock.url, Some("secret"), &[])
            .await
            .unwrap();
        event_queue::enqueue(&pool, "deployment.status_changed", "{\"a\":1}")
            .await
            .unwrap();

        process_due(&pool).await.unwrap();

        assert_eq!(mock.hits.load(Ordering::SeqCst), 1, "one delivery");
        assert_eq!(mock.signed.load(Ordering::SeqCst), 1, "carried signature");
        let due = event_queue::fetch_due(&pool, 10).await.unwrap();
        assert!(due.is_empty(), "event should be delivered, not pending");
    }

    #[tokio::test]
    async fn unsubscribed_kind_is_not_delivered() {
        let pool = test_pool().await;
        let mock = start_mock(|_| 200).await;
        webhook::create(&pool, &mock.url, Some("s"), &["other.kind".to_string()])
            .await
            .unwrap();
        event_queue::enqueue(&pool, "deployment.status_changed", "{}")
            .await
            .unwrap();

        process_due(&pool).await.unwrap();

        assert_eq!(mock.hits.load(Ordering::SeqCst), 0, "filter excludes it");
        // No subscriber matched → event is marked delivered (nothing to do).
        assert!(event_queue::fetch_due(&pool, 10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn failure_reschedules_not_delivered() {
        let pool = test_pool().await;
        let mock = start_mock(|_| 500).await;
        webhook::create(&pool, &mock.url, None, &[]).await.unwrap();
        event_queue::enqueue(&pool, "deployment.status_changed", "{}")
            .await
            .unwrap();

        process_due(&pool).await.unwrap();

        assert_eq!(mock.hits.load(Ordering::SeqCst), 1);
        // Rescheduled with a future next_attempt_at, so not currently due, but
        // still pending (not dead) after a single failure.
        let due_now = event_queue::fetch_due(&pool, 10).await.unwrap();
        assert!(due_now.is_empty(), "backed off, not due yet");
    }

    #[tokio::test]
    async fn worker_dead_letters_after_max_attempts() {
        // Drive the worker (not the model directly) to the dead-letter branch:
        // a permanently-failing subscriber on its final attempt must transition
        // the event to `dead`, never reschedule it. Guards the reschedule-vs-
        // dead-letter decision in `deliver_event`.
        let pool = test_pool().await;
        let mock = start_mock(|_| 500).await;
        webhook::create(&pool, &mock.url, None, &[]).await.unwrap();
        event_queue::enqueue(&pool, "deployment.status_changed", "{}")
            .await
            .unwrap();
        let id = event_queue::fetch_due(&pool, 10).await.unwrap()[0]
            .id
            .clone();

        // Seed the row as if MAX_ATTEMPTS-1 attempts already failed and it is
        // due now, so the next failure is the one that crosses the threshold.
        let now = chrono::Utc::now().to_rfc3339();
        sqlx::query("UPDATE events SET attempts = ?, next_attempt_at = ? WHERE id = ?")
            .bind(MAX_ATTEMPTS - 1)
            .bind(&now)
            .bind(&id)
            .execute(&pool)
            .await
            .unwrap();

        process_due(&pool).await.unwrap();

        assert_eq!(mock.hits.load(Ordering::SeqCst), 1, "one final attempt");
        // Terminal: dead, and never claimed again.
        let status: String = sqlx::query_scalar("SELECT status FROM events WHERE id = ?")
            .bind(&id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(status, "dead", "crossed MAX_ATTEMPTS → dead-lettered");
        assert!(event_queue::fetch_due(&pool, 10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn subscriber_receives_only_subscribed_kind() {
        // A hook filtered to one kind receives that kind and is NOT hit by a
        // different kind enqueued alongside it. Proves the positive and negative
        // of the subscription filter through the full worker path.
        let pool = test_pool().await;
        let mock = start_mock(|_| 200).await;
        webhook::create(
            &pool,
            &mock.url,
            Some("s"),
            &["deployment.status_changed".to_string()],
        )
        .await
        .unwrap();
        event_queue::enqueue(&pool, "deployment.status_changed", "{}")
            .await
            .unwrap();
        event_queue::enqueue(&pool, "other.kind", "{}")
            .await
            .unwrap();

        process_due(&pool).await.unwrap();

        // Exactly one delivery: the subscribed kind. The other kind matched no
        // subscriber and was marked delivered without a POST.
        assert_eq!(mock.hits.load(Ordering::SeqCst), 1, "only subscribed kind");
        assert!(event_queue::fetch_due(&pool, 10).await.unwrap().is_empty());
    }
}
