//! Per-deployment retry backoff for the scheduler.
//!
//! When a runtime fails to apply a deployment (image pull, VM boot, etc.) the
//! scheduler should not retry on the next 1s tick — that burns CPU and clouds
//! logs. Instead, each failure schedules the next attempt with exponential
//! backoff (1, 2, 4, 8, 16, 32, capped at 60s). The state lives in the
//! scheduler so every runtime benefits without duplicating the logic.
//!
//! State is intentionally non-persistent: at process restart all deployments
//! get a fresh attempt, which is fine because the scheduler will requeue them
//! anyway and any in-flight failure will simply re-fail and re-arm.

use std::collections::HashMap;
use std::time::{Duration, Instant};

const MAX_BACKOFF_SECS: u64 = 60;

#[derive(Default)]
pub(crate) struct RetryBackoff {
    next_attempt: HashMap<String, Instant>,
}

impl RetryBackoff {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// True if the deployment is still inside its backoff window and should
    /// be skipped this cycle.
    pub(crate) fn is_blocked(&self, deployment_id: &str) -> bool {
        self.next_attempt
            .get(deployment_id)
            .map(|next| Instant::now() < *next)
            .unwrap_or(false)
    }

    /// Schedule the next retry. `attempt` is `restart_count` (already
    /// incremented by the runtime). attempt=1 → 1s, 2 → 2s, 3 → 4s, …,
    /// capped at MAX_BACKOFF_SECS.
    pub(crate) fn arm(&mut self, deployment_id: &str, attempt: u32) {
        let secs = 1u64
            .checked_shl(attempt.saturating_sub(1))
            .unwrap_or(MAX_BACKOFF_SECS)
            .min(MAX_BACKOFF_SECS);
        self.next_attempt.insert(
            deployment_id.to_string(),
            Instant::now() + Duration::from_secs(secs),
        );
    }

    /// Drop any pending backoff for a deployment (success, terminal status,
    /// or deletion).
    pub(crate) fn clear(&mut self, deployment_id: &str) {
        self.next_attempt.remove(deployment_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_deployment_is_not_blocked() {
        let b = RetryBackoff::new();
        assert!(!b.is_blocked("nope"));
    }

    #[test]
    fn arm_blocks_until_window_elapses() {
        let mut b = RetryBackoff::new();
        b.arm("d1", 1);
        assert!(b.is_blocked("d1"));
    }

    #[test]
    fn clear_unblocks() {
        let mut b = RetryBackoff::new();
        b.arm("d1", 5);
        b.clear("d1");
        assert!(!b.is_blocked("d1"));
    }

    #[test]
    fn backoff_doubles_then_caps() {
        // We can't easily test the actual delay without sleeping, but we can
        // check the arithmetic via the arm function's effect on stored time.
        let mut b = RetryBackoff::new();
        let before = Instant::now();
        b.arm("d1", 10); // 2^9 = 512, capped to 60
        let next = *b.next_attempt.get("d1").unwrap();
        let delta = next.duration_since(before);
        assert!(delta <= Duration::from_secs(MAX_BACKOFF_SECS + 1));
        assert!(delta >= Duration::from_secs(MAX_BACKOFF_SECS - 1));
    }
}
