//! Horizontal autoscaling: pick a deployment's instance count from its observed
//! CPU usage.
//!
//! The decision is deliberately conservative, because the cost of a wrong one is
//! asymmetric: scaling up late costs latency for a few seconds, scaling up and
//! down repeatedly (flapping) costs container churn, cold starts, and log noise
//! for as long as nobody notices.
//!
//! Three mechanisms keep it calm:
//!
//! * **One step at a time.** A decision moves the count by ±1, never straight to
//!   a computed target. The runtimes already create one container per tick, so
//!   this matches how the reconciliation loop behaves anyway.
//! * **A dead band.** Usage within a tolerance of the target is "on target", so
//!   normal jitter around the setpoint decides nothing.
//! * **An asymmetric cooldown.** Scaling up is allowed sooner than scaling down:
//!   under-provisioning hurts users, over-provisioning only costs resources, and
//!   a slow scale-down is what stops a load that oscillates around the target
//!   from driving the instance count up and down with it.
//!
//! State is in-memory and non-persistent, like [`super::healthy_window`] and
//! [`super::backoff`]. At process restart the cooldowns start over: the first
//! post-restart tick may act immediately, which is safe — the decision is still
//! bounded by the policy and by one step.

use crate::models::deployments::Autoscale;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// How far from `target_cpu` still counts as "on target", in percentage points.
///
/// Without this, a deployment sitting exactly at its target scales up and down
/// forever, because measured CPU is never exactly equal to anything.
const DEAD_BAND_PERCENT: f64 = 10.0;

/// Minimum time between a scale-up and the next decision.
const SCALE_UP_COOLDOWN: Duration = Duration::from_secs(60);

/// Minimum time between a scale-down and the next decision. Longer than the
/// scale-up cooldown on purpose: shedding capacity too eagerly is what turns a
/// fluctuating load into a flapping instance count.
const SCALE_DOWN_COOLDOWN: Duration = Duration::from_secs(300);

/// What the autoscaler decided for one deployment on one tick.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(crate) enum Decision {
    /// Change the instance count to this value. Already clamped to the policy.
    ScaleTo(u32),
    /// Leave it alone.
    Hold,
}

/// Per-deployment cooldown clock.
#[derive(Default)]
pub(crate) struct Autoscaler {
    /// `deployment_id -> when the last scaling action was taken`.
    last_action: HashMap<String, Instant>,
}

impl Autoscaler {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Decide what to do with one deployment.
    ///
    /// `cpu_percent` must be the **per-instance average**, not the deployment
    /// total. `stats_cache` sums CPU across instances (correct for the
    /// Prometheus gauge, wrong as a setpoint), so the caller divides by the
    /// instance count — see [`average_cpu`]. Comparing a sum against a target
    /// would be a runaway: three instances at 30% sum to 90%, which "exceeds" a
    /// 70% target, so the autoscaler adds a fourth, pushing the sum higher
    /// still, all the way to `max`, while every instance sits idle.
    ///
    /// `cpu_percent` is `None` when no usable measurement exists (the runtime
    /// was unreachable, the deployment has no running instance yet, or the
    /// stats snapshot has not covered it). In that case the answer is always
    /// [`Decision::Hold`]: acting on a missing measurement would mean scaling
    /// blind, and "no data" most often means "nothing is running", where
    /// scaling down is exactly wrong.
    pub(crate) fn decide(
        &mut self,
        deployment_id: &str,
        policy: &Autoscale,
        current: u32,
        cpu_percent: Option<f64>,
        now: Instant,
    ) -> Decision {
        // A policy edited to a narrower range takes effect even while usage
        // sits in the dead band, so a deployment left outside its own bounds
        // converges instead of waiting for a load change.
        let clamped = policy.clamp(current);
        if clamped != current {
            self.last_action.insert(deployment_id.to_string(), now);
            return Decision::ScaleTo(clamped);
        }

        let Some(cpu) = cpu_percent.filter(|c| c.is_finite() && *c >= 0.0) else {
            return Decision::Hold;
        };

        let above = cpu > policy.target_cpu + DEAD_BAND_PERCENT;
        let below = cpu < policy.target_cpu - DEAD_BAND_PERCENT;

        let target = if above {
            current.saturating_add(1)
        } else if below {
            current.saturating_sub(1)
        } else {
            return Decision::Hold;
        };

        // Already at the bound the load is pushing against: nothing to do, and
        // no cooldown to spend.
        let target = policy.clamp(target);
        if target == current {
            return Decision::Hold;
        }

        let cooldown = if target > current {
            SCALE_UP_COOLDOWN
        } else {
            SCALE_DOWN_COOLDOWN
        };

        if let Some(last) = self.last_action.get(deployment_id)
            && now.duration_since(*last) < cooldown
        {
            return Decision::Hold;
        }

        self.last_action.insert(deployment_id.to_string(), now);
        Decision::ScaleTo(target)
    }

    /// Drop bookkeeping for deployments that no longer exist, so the map does
    /// not grow for the life of the process.
    pub(crate) fn retain_known(&mut self, live_ids: &[String]) {
        self.last_action.retain(|id, _| live_ids.contains(id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(min: u32, max: u32, target_cpu: f64) -> Autoscale {
        Autoscale {
            min,
            max,
            target_cpu,
        }
    }

    #[test]
    fn scales_up_when_cpu_is_above_the_dead_band() {
        let mut a = Autoscaler::new();
        let d = a.decide("d", &policy(1, 5, 70.0), 2, Some(95.0), Instant::now());
        assert_eq!(d, Decision::ScaleTo(3), "high CPU must add one instance");
    }

    #[test]
    fn scales_down_when_cpu_is_below_the_dead_band() {
        let mut a = Autoscaler::new();
        let d = a.decide("d", &policy(1, 5, 70.0), 3, Some(10.0), Instant::now());
        assert_eq!(d, Decision::ScaleTo(2), "idle CPU must remove one instance");
    }

    #[test]
    fn holds_inside_the_dead_band() {
        let mut a = Autoscaler::new();
        // 72% against a 70% target: normal jitter, not a signal.
        let d = a.decide("d", &policy(1, 5, 70.0), 3, Some(72.0), Instant::now());
        assert_eq!(d, Decision::Hold);
    }

    #[test]
    fn moves_one_step_at_a_time_even_when_far_off_target() {
        // Massively overloaded: still +1, not a jump to max. The next ticks
        // keep climbing while the load stays high.
        let mut a = Autoscaler::new();
        let d = a.decide("d", &policy(1, 10, 50.0), 2, Some(100.0), Instant::now());
        assert_eq!(d, Decision::ScaleTo(3));
    }

    #[test]
    fn never_leaves_the_policy_bounds() {
        let mut a = Autoscaler::new();
        let now = Instant::now();
        // At max under heavy load: hold, and do not burn the cooldown.
        assert_eq!(
            a.decide("up", &policy(1, 3, 70.0), 3, Some(99.0), now),
            Decision::Hold
        );
        // At min while idle: hold.
        assert_eq!(
            a.decide("down", &policy(2, 5, 70.0), 2, Some(1.0), now),
            Decision::Hold
        );
    }

    #[test]
    fn missing_metrics_never_scale() {
        // "No data" usually means nothing is running yet — scaling down there
        // would be exactly backwards, and scaling up would be guessing.
        let mut a = Autoscaler::new();
        let now = Instant::now();
        assert_eq!(
            a.decide("d", &policy(1, 5, 70.0), 3, None, now),
            Decision::Hold
        );
        assert_eq!(
            a.decide("d", &policy(1, 5, 70.0), 3, Some(f64::NAN), now),
            Decision::Hold
        );
    }

    #[test]
    fn cooldown_blocks_a_second_decision() {
        let mut a = Autoscaler::new();
        let t0 = Instant::now();
        assert_eq!(
            a.decide("d", &policy(1, 9, 70.0), 2, Some(99.0), t0),
            Decision::ScaleTo(3)
        );
        // One second later the load is still high, but the cooldown holds.
        let t1 = t0 + Duration::from_secs(1);
        assert_eq!(
            a.decide("d", &policy(1, 9, 70.0), 3, Some(99.0), t1),
            Decision::Hold
        );
        // Past the cooldown it may act again.
        let t2 = t0 + SCALE_UP_COOLDOWN + Duration::from_secs(1);
        assert_eq!(
            a.decide("d", &policy(1, 9, 70.0), 3, Some(99.0), t2),
            Decision::ScaleTo(4)
        );
    }

    #[test]
    fn scaling_down_waits_longer_than_scaling_up() {
        let mut a = Autoscaler::new();
        let t0 = Instant::now();
        assert_eq!(
            a.decide("d", &policy(1, 9, 70.0), 4, Some(5.0), t0),
            Decision::ScaleTo(3)
        );
        // The scale-up cooldown has elapsed, but a scale-down needs longer.
        let t1 = t0 + SCALE_UP_COOLDOWN + Duration::from_secs(1);
        assert_eq!(
            a.decide("d", &policy(1, 9, 70.0), 3, Some(5.0), t1),
            Decision::Hold,
            "shedding capacity must not be as eager as adding it"
        );
        let t2 = t0 + SCALE_DOWN_COOLDOWN + Duration::from_secs(1);
        assert_eq!(
            a.decide("d", &policy(1, 9, 70.0), 3, Some(5.0), t2),
            Decision::ScaleTo(2)
        );
    }

    #[test]
    fn a_narrowed_policy_pulls_the_count_back_into_range() {
        // max lowered from 10 to 4 while 8 instances run: converge immediately,
        // without waiting for the load to move.
        let mut a = Autoscaler::new();
        let d = a.decide("d", &policy(1, 4, 70.0), 8, Some(72.0), Instant::now());
        assert_eq!(d, Decision::ScaleTo(4));
    }

    #[test]
    fn oscillating_load_does_not_flap() {
        // The scenario the cooldowns exist for: load alternates high/low every
        // tick. What must NOT happen is the count following it up and down;
        // climbing under repeated overload is the intended behaviour.
        let mut a = Autoscaler::new();
        let p = policy(1, 10, 50.0);
        let t0 = Instant::now();
        let mut current = 3;
        let mut ups = 0;
        let mut downs = 0;

        for tick in 0..20 {
            let cpu = if tick % 2 == 0 { 95.0 } else { 5.0 };
            let now = t0 + Duration::from_secs(tick * 10);
            if let Decision::ScaleTo(n) = a.decide("d", &p, current, Some(cpu), now) {
                if n > current {
                    ups += 1;
                } else {
                    downs += 1;
                }
                current = n;
            }
        }

        // The long scale-down cooldown is what breaks the oscillation: within
        // 200s the spikes may add capacity, but the dips never take it away, so
        // the count never chases the load back and forth.
        assert_eq!(
            downs, 0,
            "an oscillating load must not shed capacity between spikes (got {downs} scale-downs)"
        );
        assert!(
            ups <= 4,
            "scale-ups should be paced by the cooldown, got {ups} in 200s"
        );
    }

    #[test]
    fn idle_instances_scale_down_rather_than_run_away() {
        // Regression guard for the bug that nearly shipped: `stats_cache` sums
        // CPU across instances, so three instances at 30% report 90%. Fed that
        // sum, the controller would read "above a 70% target", add an instance,
        // see the sum rise, and climb to max while every instance idled.
        //
        // `decide` takes the PER-INSTANCE average, so the same workload reads
        // as 30% and correctly sheds capacity.
        let mut a = Autoscaler::new();
        let p = policy(1, 10, 70.0);
        let t0 = Instant::now();
        let mut current = 3;

        for tick in 0..10 {
            let per_instance = 30.0;
            let now = t0 + Duration::from_secs(tick * 120);
            if let Decision::ScaleTo(n) = a.decide("d", &p, current, Some(per_instance), now) {
                current = n;
            }
        }

        assert!(
            current < 3,
            "instances idling at 30% against a 70% target must scale DOWN, got {current}"
        );
    }

    #[test]
    fn retain_known_drops_deleted_deployments() {
        let mut a = Autoscaler::new();
        let now = Instant::now();
        a.decide("gone", &policy(1, 5, 70.0), 2, Some(99.0), now);
        a.decide("kept", &policy(1, 5, 70.0), 2, Some(99.0), now);

        a.retain_known(&["kept".to_string()]);

        assert!(!a.last_action.contains_key("gone"));
        assert!(a.last_action.contains_key("kept"));
    }
}
