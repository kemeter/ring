//! Windowed reset of a deployment's `restart_count`.
//!
//! `restart_count` is otherwise monotonic for the life of a deployment: a worker
//! that crashed a few times early on, then ran healthy for weeks, keeps the old
//! count, so the next single crash trips `CrashLoopBackOff`. That turns the
//! intended "5 crashes within a window" semantics into "5 crashes ever".
//!
//! This tracker watches how long a worker has been continuously `Running` with a
//! non-zero `restart_count`. Once it has stayed healthy for at least the
//! anti-flap window (`min_healthy_time`, same window the rollout readiness gate
//! uses), the count is reset to 0 — the crash budget refills.
//!
//! State is in-memory and non-persistent, like [`super::backoff::RetryBackoff`]:
//! at process restart the clock starts over, which is safe (a still-crashing
//! worker re-crashes and never accrues the window; a healthy one simply takes
//! one window longer to have its old count forgiven).

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Per-deployment "continuously running since" clock, scoped to deployments
/// whose `restart_count` is non-zero (a zero count needs no reset).
#[derive(Default)]
pub(crate) struct HealthyWindow {
    /// `deployment_id -> (running_since, restart_count_observed_at_that_time)`.
    running_since: HashMap<String, (Instant, u32)>,
}

impl HealthyWindow {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Record this tick's observation and decide whether `restart_count` should
    /// be reset to 0. Returns `true` exactly once, on the tick where the worker
    /// has been continuously `Running` (with an unchanged non-zero count) for at
    /// least `window`; the caller is then responsible for persisting the reset.
    ///
    /// The clock restarts whenever the worker is not `Running` or its
    /// `restart_count` changed since the last observation (a fresh crash), so
    /// only an uninterrupted healthy stretch counts toward the window.
    pub(crate) fn observe(
        &mut self,
        deployment_id: &str,
        is_running: bool,
        restart_count: u32,
        window: Duration,
    ) -> bool {
        // Nothing to forgive, and not healthy-running: drop any clock and move on.
        if restart_count == 0 || !is_running {
            self.running_since.remove(deployment_id);
            return false;
        }

        let now = Instant::now();
        match self.running_since.get(deployment_id).copied() {
            // Same count, still running: the window keeps accruing.
            Some((since, observed)) if observed == restart_count => {
                if now.duration_since(since) >= window {
                    self.running_since.remove(deployment_id);
                    return true;
                }
                false
            }
            // First healthy observation, or the count moved (a fresh crash):
            // (re)start the clock from now.
            _ => {
                self.running_since
                    .insert(deployment_id.to_string(), (now, restart_count));
                false
            }
        }
    }

    /// Forget a deployment (deleted, or its count was reset elsewhere).
    pub(crate) fn clear(&mut self, deployment_id: &str) {
        self.running_since.remove(deployment_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WINDOW: Duration = Duration::from_secs(10);

    #[test]
    fn zero_count_never_resets() {
        let mut w = HealthyWindow::new();
        assert!(!w.observe("d1", true, 0, WINDOW));
    }

    #[test]
    fn not_running_clears_the_clock() {
        let mut w = HealthyWindow::new();
        // Start the clock while running with a non-zero count.
        assert!(!w.observe("d1", true, 4, WINDOW));
        // A non-running tick must drop the clock so a later running stretch
        // starts fresh, not from the original timestamp.
        assert!(!w.observe("d1", false, 4, WINDOW));
        assert!(!w.running_since.contains_key("d1"));
    }

    #[test]
    fn resets_after_window_with_stable_count() {
        let mut w = HealthyWindow::new();
        // First observation arms the clock in the past so the window has elapsed.
        w.running_since
            .insert("d1".to_string(), (Instant::now() - WINDOW, 4));
        assert!(w.observe("d1", true, 4, WINDOW));
        // And it only fires once: the entry is gone afterwards.
        assert!(!w.running_since.contains_key("d1"));
    }

    #[test]
    fn does_not_reset_before_window() {
        let mut w = HealthyWindow::new();
        // Clock just armed: well within the window, no reset yet.
        assert!(!w.observe("d1", true, 4, WINDOW));
        assert!(!w.observe("d1", true, 4, WINDOW));
    }

    #[test]
    fn a_fresh_crash_restarts_the_clock() {
        let mut w = HealthyWindow::new();
        // Healthy for a full window with count=4 would normally reset...
        w.running_since
            .insert("d1".to_string(), (Instant::now() - WINDOW, 4));
        // ...but the count moved to 5 this tick (a new crash): the window must
        // restart from now, so this tick does NOT reset.
        assert!(!w.observe("d1", true, 5, WINDOW));
        let (_, observed) = *w.running_since.get("d1").unwrap();
        assert_eq!(observed, 5);
    }

    /// End-to-end Phase 1 semantics: a worker crashes its way up to count=4,
    /// then runs healthy. Within the window no reset fires; once it has been
    /// continuously healthy past the window the count is forgiven (reset → true).
    #[test]
    fn crash_four_times_then_healthy_past_window_resets() {
        let mut w = HealthyWindow::new();

        // Four crashes: each is a fresh count, so the clock keeps restarting and
        // never resets while the worker is still crashing.
        for count in 1..=4 {
            assert!(!w.observe("d1", true, count, WINDOW));
        }

        // Healthy stretch begins at count=4. Just inside the window: no reset.
        assert!(!w.observe("d1", true, 4, WINDOW));

        // Backdate the clock so the window has fully elapsed with a stable count.
        let (_, observed) = *w.running_since.get("d1").unwrap();
        w.running_since
            .insert("d1".to_string(), (Instant::now() - WINDOW, observed));

        // Now healthy past the window: the budget refills.
        assert!(w.observe("d1", true, 4, WINDOW));
    }

    /// Crashes that keep arriving inside the window never reset, so the count is
    /// free to climb to the CrashLoopBackOff bound — the reset can't mask a loop.
    #[test]
    fn crashes_within_window_never_reset() {
        let mut w = HealthyWindow::new();
        for count in 1..=5 {
            // Even if a stale clock from a prior count had elapsed, a changed
            // count restarts it, so no reset ever fires mid-loop.
            assert!(!w.observe("d1", true, count, WINDOW));
        }
    }

    #[test]
    fn clear_drops_the_entry() {
        let mut w = HealthyWindow::new();
        w.observe("d1", true, 3, WINDOW);
        w.clear("d1");
        assert!(!w.running_since.contains_key("d1"));
    }
}
