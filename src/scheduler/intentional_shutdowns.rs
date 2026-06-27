//! Tracks container shutdowns that Ring itself initiated, so the scheduler can
//! tell them apart from real crashes when it processes Docker `die` events.
//!
//! # Why
//!
//! A container that stops shows up two ways: Docker emits a `die` event, and the
//! reconcile pass sees it as an `exited` instance. Either can stem from a crash,
//! an OOM kill, or a graceful `docker stop` we sent ourselves. The reconcile
//! pass (`detect_and_count_crashes`) counts each unexpected exit toward
//! `restart_count`; once it reaches `MAX_RESTART_COUNT` the deployment flips to
//! `CrashLoopBackOff`. Without this filter, every scale-down, delete, rolling
//! update step or health-check eviction would be counted as a crash and could
//! push a perfectly healthy deployment into `CrashLoopBackOff`.
//!
//! # How it works
//!
//! Before the runtime stops a container on purpose, it calls
//! [`IntentionalShutdowns::mark`] with the container id. Two consumers call
//! [`IntentionalShutdowns::take`]: `detect_and_count_crashes` skips the exited
//! container so it is reaped without bumping `restart_count`, and
//! `apply_docker_event` skips the matching `die` event so it is not logged as a
//! crash. The first to observe the stop consumes the entry.
//!
//! # Where to mark
//!
//! Mark every Ring-initiated stop. Today that's:
//! - scale-down (`runtime/docker/lifecycle.rs`)
//! - delete / `remove_all_instances` (`runtime/docker/lifecycle.rs`)
//! - rolling update + health-check eviction, both via `remove_instance`
//!   (`runtime/docker/docker_lifecycle.rs`)
//!
//! Do NOT mark a container when the *container itself* failed (a real crash,
//! an OOM, an exit). Those must reach `detect_and_count_crashes` so they count
//! toward `restart_count`.
//!
//! # TTL
//!
//! Entries auto-expire after [`ENTRY_TTL`] so a forgotten mark (Docker never
//! emits the matching `die`, the daemon was restarted, etc.) cannot live
//! forever in memory or accidentally absorb a future crash on a recycled id.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

const ENTRY_TTL: Duration = Duration::from_secs(60);

#[derive(Clone, Default)]
pub(crate) struct IntentionalShutdowns {
    inner: Arc<Mutex<HashMap<String, Instant>>>,
}

impl IntentionalShutdowns {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) async fn mark(&self, container_id: String) {
        let mut guard = self.inner.lock().await;
        prune(&mut guard);
        guard.insert(container_id, Instant::now());
    }

    pub(crate) async fn take(&self, container_id: &str) -> bool {
        let mut guard = self.inner.lock().await;
        prune(&mut guard);
        guard.remove(container_id).is_some()
    }
}

fn prune(map: &mut HashMap<String, Instant>) {
    let now = Instant::now();
    map.retain(|_, ts| now.duration_since(*ts) < ENTRY_TTL);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mark_then_take_returns_true_once() {
        let reg = IntentionalShutdowns::new();
        reg.mark("abc".to_string()).await;
        assert!(reg.take("abc").await);
        assert!(!reg.take("abc").await);
    }

    #[tokio::test]
    async fn take_unknown_returns_false() {
        let reg = IntentionalShutdowns::new();
        assert!(!reg.take("missing").await);
    }

    #[tokio::test]
    async fn entries_expire_after_ttl() {
        let reg = IntentionalShutdowns::new();
        {
            let mut guard = reg.inner.lock().await;
            guard.insert(
                "stale".to_string(),
                Instant::now() - Duration::from_secs(120),
            );
        }
        assert!(!reg.take("stale").await);
    }
}
