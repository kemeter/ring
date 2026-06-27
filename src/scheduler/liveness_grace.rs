//! Per-deployment liveness grace window.
//!
//! When a deployment first enters `Running` (the readiness gate has finally let
//! the optimistic `Creating → Running` flip stand), the liveness health checks
//! start acting on `old_status == Running`. With zero settle margin, a single
//! brief flap of the liveness probe right after promotion can fire its
//! `on_failure: restart` and tear the instance down — which, for a runtime that
//! runs a slow in-container build at startup (e.g. `bun run build` behind
//! Caddy), restarts the whole build from scratch and loops until the rollout
//! deadline marks the deployment `failed`.
//!
//! This tracker remembers *when* a deployment first became `Running` and lets
//! the scheduler suppress the liveness `on_failure` actions until it has been
//! `Running` for at least the grace window. The probe still runs and its result
//! is still recorded; only the restart/stop/alert action is held back during
//! the settle period.
//!
//! Crucially this only gates the liveness *health-check* action path. A
//! container that genuinely EXITS (Docker `die` event → crash path /
//! liveness-confirmed Running re-inspect) is handled elsewhere and is NOT
//! suppressed by this grace: a real crash is still recreated immediately. The
//! grace only filters a *probe* that flaps while the app is settling, never an
//! exit.
//!
//! State is in-memory and non-persistent, like [`super::backoff::RetryBackoff`]
//! and [`super::healthy_window::HealthyWindow`]: at process restart the clock
//! starts over, which is safe — a deployment that is already `Running` simply
//! gets one fresh grace window, and a genuinely-dead container is still caught
//! by the exit-based path that this tracker never touches.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Built-in liveness grace window. Ordered deliberately below the sauron
/// `ready` probe timeout (300s) and the rollout deadline (600s) and above a
/// typical in-container build (33-60s): `grace (120) < ready_timeout (300) <
/// rollout_deadline (600)`. Overridable via `RING_LIVENESS_GRACE` (seconds).
pub(crate) const DEFAULT_LIVENESS_GRACE: Duration = Duration::from_secs(120);

/// Resolve the liveness grace from the environment, falling back to
/// [`DEFAULT_LIVENESS_GRACE`]. A value of `0` disables the grace (useful for
/// deterministic tests and for operators who explicitly opt out).
pub(crate) fn liveness_grace_from_env() -> Duration {
    match std::env::var("RING_LIVENESS_GRACE") {
        Ok(raw) => match raw.parse::<u64>() {
            Ok(secs) => Duration::from_secs(secs),
            Err(_) => DEFAULT_LIVENESS_GRACE,
        },
        Err(_) => DEFAULT_LIVENESS_GRACE,
    }
}

/// Per-deployment "running since" clock used to suppress liveness actions
/// during the settle window after a deployment first becomes `Running`.
#[derive(Default)]
pub(crate) struct LivenessGrace {
    /// `deployment_id -> Instant the deployment was first observed Running`.
    running_since: HashMap<String, Instant>,
}

impl LivenessGrace {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Record this tick's observation and decide whether the deployment is
    /// still inside its liveness grace window (so liveness `on_failure` actions
    /// must be held back).
    ///
    /// - The clock is armed the first tick a deployment is observed `Running`.
    /// - While not `Running` the clock is dropped, so a later `Running` stretch
    ///   (a recreate, or a gate that reverted to `Creating`) starts a fresh
    ///   grace window rather than counting from a stale timestamp.
    ///
    /// Returns `true` while the deployment has been `Running` for less than
    /// `grace` — i.e. liveness restarts should be suppressed this tick.
    pub(crate) fn in_grace(
        &mut self,
        deployment_id: &str,
        is_running: bool,
        grace: Duration,
    ) -> bool {
        if !is_running {
            self.running_since.remove(deployment_id);
            return false;
        }

        let now = Instant::now();
        let since = *self
            .running_since
            .entry(deployment_id.to_string())
            .or_insert(now);

        now.duration_since(since) < grace
    }

    /// Forget a deployment (deleted, or being recreated). The next `Running`
    /// observation re-arms a fresh grace window.
    pub(crate) fn clear(&mut self, deployment_id: &str) {
        self.running_since.remove(deployment_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GRACE: Duration = Duration::from_secs(120);

    #[test]
    fn not_running_is_never_in_grace() {
        let mut g = LivenessGrace::new();
        assert!(!g.in_grace("d1", false, GRACE));
        assert!(!g.running_since.contains_key("d1"));
    }

    #[test]
    fn first_running_tick_is_in_grace() {
        let mut g = LivenessGrace::new();
        // Just became Running: well inside the window, suppress liveness.
        assert!(g.in_grace("d1", true, GRACE));
    }

    #[test]
    fn no_longer_in_grace_after_window_elapses() {
        let mut g = LivenessGrace::new();
        // Arm the clock in the past so the window has fully elapsed.
        g.running_since
            .insert("d1".to_string(), Instant::now() - GRACE);
        assert!(!g.in_grace("d1", true, GRACE));
    }

    #[test]
    fn leaving_running_resets_the_clock() {
        let mut g = LivenessGrace::new();
        // Arm an elapsed clock, then a non-Running tick must drop it...
        g.running_since
            .insert("d1".to_string(), Instant::now() - GRACE);
        assert!(!g.in_grace("d1", false, GRACE));
        // ...so the next Running tick starts a *fresh* grace window.
        assert!(g.in_grace("d1", true, GRACE));
    }

    #[test]
    fn zero_grace_never_suppresses() {
        let mut g = LivenessGrace::new();
        assert!(!g.in_grace("d1", true, Duration::from_secs(0)));
    }

    #[test]
    fn clear_drops_the_entry() {
        let mut g = LivenessGrace::new();
        assert!(g.in_grace("d1", true, GRACE));
        g.clear("d1");
        assert!(!g.running_since.contains_key("d1"));
    }

    #[test]
    fn env_override_parses_seconds() {
        // Default when unset.
        unsafe {
            std::env::remove_var("RING_LIVENESS_GRACE");
        }
        assert_eq!(liveness_grace_from_env(), DEFAULT_LIVENESS_GRACE);

        unsafe {
            std::env::set_var("RING_LIVENESS_GRACE", "5");
        }
        assert_eq!(liveness_grace_from_env(), Duration::from_secs(5));

        // Malformed falls back to the default rather than panicking.
        unsafe {
            std::env::set_var("RING_LIVENESS_GRACE", "not-a-number");
        }
        assert_eq!(liveness_grace_from_env(), DEFAULT_LIVENESS_GRACE);

        unsafe {
            std::env::remove_var("RING_LIVENESS_GRACE");
        }
    }
}
