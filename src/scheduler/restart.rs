//! Restart policy: when to restart a workload, how long to wait between
//! attempts, and when to give up.
//!
//! This module only decides. It does no I/O and reads no configuration: the
//! caller hands it a resolved [`RestartPolicy`], the deployment's
//! [`RestartState`] and what just happened, and gets a [`Decision`] back.
//!
//! - Workers restart on any exit and any start error, and by default never
//!   give up: they keep retrying at `cap`. `OnExhaustion::Fail` opts into
//!   failing after `max_attempts`.
//! - Jobs complete on exit 0 and fail once the runs that exited non-zero
//!   exceed `backoff_limit` (0 by default: a job that ran and failed is not run
//!   again unless its manifest asks for it). A job that could not even start
//!   ran nothing, so it is retried without limit, as Kubernetes does.
//! - The delay before attempt `n` is drawn with full jitter from
//!   `[0, min(cap, base * 2^(n - 1))]`, so workloads failing together (a host
//!   reboot) do not retry together.
//! - The attempt counter resets once an instance has been running for
//!   `stable_after`.
//!
//! See `documentation/design/restart-policy.md`.

use chrono::{DateTime, Utc};
use rand::Rng;
use std::time::Duration;

pub(crate) const DEFAULT_BASE: Duration = Duration::from_secs(10);
pub(crate) const DEFAULT_CAP: Duration = Duration::from_secs(5 * 60);
pub(crate) const DEFAULT_STABLE_AFTER: Duration = Duration::from_secs(10 * 60);
pub(crate) const DEFAULT_MAX_ATTEMPTS: u32 = 5;
/// Runs a failed job gets after its first, unless its manifest says otherwise.
/// Zero, because a job that half-ran (a migration, a dump) is not safe to run
/// again without being told so.
pub(crate) const DEFAULT_BACKOFF_LIMIT: u32 = 0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorkloadKind {
    Worker,
    Job { backoff_limit: u32 },
}

/// What a worker does once its failures would exceed `max_attempts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OnExhaustion {
    /// Keep retrying at `cap`, forever.
    Backoff,
    /// Stop and mark the deployment failed.
    Fail,
}

impl std::str::FromStr for OnExhaustion {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "backoff" => Ok(OnExhaustion::Backoff),
            "fail" => Ok(OnExhaustion::Fail),
            other => Err(format!("'{other}' is not one of \"backoff\", \"fail\"")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RestartPolicy {
    pub(crate) base: Duration,
    pub(crate) cap: Duration,
    pub(crate) stable_after: Duration,
    pub(crate) on_exhaustion: OnExhaustion,
    pub(crate) max_attempts: u32,
}

impl Default for RestartPolicy {
    fn default() -> Self {
        Self {
            base: DEFAULT_BASE,
            cap: DEFAULT_CAP,
            stable_after: DEFAULT_STABLE_AFTER,
            on_exhaustion: OnExhaustion::Backoff,
            max_attempts: DEFAULT_MAX_ATTEMPTS,
        }
    }
}

/// The `restart` block of a manifest. Every key is optional and falls back to
/// the server policy.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct RestartOverride {
    pub(crate) base: Option<Duration>,
    pub(crate) cap: Option<Duration>,
    pub(crate) stable_after: Option<Duration>,
    pub(crate) on_exhaustion: Option<OnExhaustion>,
    pub(crate) max_attempts: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum PolicyError {
    #[error("restart.base must be greater than zero")]
    ZeroBase,
    #[error("restart.cap ({cap:?}) must not be lower than restart.base ({base:?})")]
    CapBelowBase { base: Duration, cap: Duration },
    #[error("restart.stable_after must be greater than zero")]
    ZeroStableAfter,
    #[error("restart.max_attempts must be at least 1")]
    ZeroMaxAttempts,
}

impl RestartPolicy {
    pub(crate) fn validate(&self) -> Result<(), PolicyError> {
        if self.base.is_zero() {
            return Err(PolicyError::ZeroBase);
        }
        if self.cap < self.base {
            return Err(PolicyError::CapBelowBase {
                base: self.base,
                cap: self.cap,
            });
        }
        if self.stable_after.is_zero() {
            return Err(PolicyError::ZeroStableAfter);
        }
        if self.max_attempts == 0 {
            return Err(PolicyError::ZeroMaxAttempts);
        }
        Ok(())
    }

    /// The policy for one deployment: this one, with the manifest's overrides
    /// applied key by key. The result is validated as a whole, so an override
    /// of `cap` alone is checked against the server's `base`.
    pub(crate) fn with_override(&self, o: &RestartOverride) -> Result<Self, PolicyError> {
        let policy = Self {
            base: o.base.unwrap_or(self.base),
            cap: o.cap.unwrap_or(self.cap),
            stable_after: o.stable_after.unwrap_or(self.stable_after),
            on_exhaustion: o.on_exhaustion.unwrap_or(self.on_exhaustion),
            max_attempts: o.max_attempts.unwrap_or(self.max_attempts),
        };
        policy.validate()?;
        Ok(policy)
    }

    /// Upper bound of the delay before attempt `attempt` (1-based):
    /// `min(cap, base * 2^(attempt - 1))`.
    pub(crate) fn delay_ceiling(&self, attempt: u32) -> Duration {
        let factor = 1u32
            .checked_shl(attempt.saturating_sub(1))
            .unwrap_or(u32::MAX);
        self.base
            .checked_mul(factor)
            .map_or(self.cap, |d| d.min(self.cap))
    }
}

/// Restart bookkeeping for one deployment, persisted between ticks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct RestartState {
    /// Failures since the last reset.
    pub(crate) restart_count: u32,
    /// Start of the current uninterrupted run, if an instance is running.
    pub(crate) running_since: Option<DateTime<Utc>>,
    /// Job runs that exited non-zero. Unlike `restart_count`, start failures
    /// do not count, and it never resets: it is what `backoff_limit` bounds.
    pub(crate) run_failures: u32,
}

/// Why an instance could not be started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StartFailure {
    /// May clear up on its own: registry, network, resources.
    Transient,
    /// Needs an operator: a missing config, a spec the runtime rejects. Still
    /// retried, but straight at `cap`.
    NeedsOperator,
}

/// What just happened to the deployment's instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Event {
    /// The instance terminated. `None` when the runtime cannot know the exit
    /// code (a VM that shut down). Liveness kills are reported here too.
    Exited { exit_code: Option<i64> },
    /// The instance could not be started.
    StartFailed(StartFailure),
}

impl Event {
    /// An exit code that says the program can never run (126 not executable,
    /// 127 not found) needs an operator just like a rejected spec does.
    fn needs_operator(&self) -> bool {
        match self {
            Event::StartFailed(failure) => *failure == StartFailure::NeedsOperator,
            Event::Exited { exit_code } => matches!(exit_code, Some(126) | Some(127)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Decision {
    StartNow,
    WaitUntil(DateTime<Utc>),
    /// Jobs only.
    Complete,
    /// Jobs past `backoff_limit`, or workers under `OnExhaustion::Fail`.
    Fail,
}

/// Records that an instance is up. Keeps the start of an ongoing run.
pub(crate) fn on_running(state: &mut RestartState, now: DateTime<Utc>) {
    state.running_since.get_or_insert(now);
}

/// Resets the attempt counter once the current run has lasted `stable_after`.
/// Returns true when it did.
pub(crate) fn reset_if_stable(
    policy: &RestartPolicy,
    state: &mut RestartState,
    now: DateTime<Utc>,
) -> bool {
    let Some(since) = state.running_since else {
        return false;
    };
    if state.restart_count == 0 {
        return false;
    }
    let Ok(stable_after) = chrono::Duration::from_std(policy.stable_after) else {
        return false;
    };
    if now - since < stable_after {
        return false;
    }
    state.restart_count = 0;
    true
}

/// Decides what to do after `event`, updating `state` accordingly.
pub(crate) fn decide(
    policy: &RestartPolicy,
    kind: WorkloadKind,
    state: &mut RestartState,
    event: Event,
    now: DateTime<Utc>,
    rng: &mut impl Rng,
) -> Decision {
    // Whatever happened, the instance is no longer running. Settle a pending
    // reset first so a crash after a long healthy run starts from attempt 1.
    reset_if_stable(policy, state, now);
    state.running_since = None;

    if let (WorkloadKind::Job { .. }, Event::Exited { exit_code: Some(0) }) = (kind, event) {
        return Decision::Complete;
    }

    state.restart_count = state.restart_count.saturating_add(1);

    let exhausted = match (kind, event) {
        (WorkloadKind::Job { backoff_limit }, Event::Exited { .. }) => {
            state.run_failures = state.run_failures.saturating_add(1);
            state.run_failures > backoff_limit
        }
        // Nothing ran: retrying a job that could not start is always safe.
        (WorkloadKind::Job { .. }, Event::StartFailed(_)) => false,
        (WorkloadKind::Worker, _) => {
            policy.on_exhaustion == OnExhaustion::Fail && state.restart_count > policy.max_attempts
        }
    };
    if exhausted {
        return Decision::Fail;
    }

    match retry_at(
        policy,
        state.restart_count,
        event.needs_operator(),
        now,
        rng,
    ) {
        Some(at) => Decision::WaitUntil(at),
        None => Decision::StartNow,
    }
}

/// When attempt `attempt` (1-based) may run, or `None` to run it now. Failures
/// that need an operator draw from the whole `[0, cap]` range from the first
/// attempt on.
pub(crate) fn retry_at(
    policy: &RestartPolicy,
    attempt: u32,
    needs_operator: bool,
    now: DateTime<Utc>,
    rng: &mut impl Rng,
) -> Option<DateTime<Utc>> {
    let ceiling = if needs_operator {
        policy.cap
    } else {
        policy.delay_ceiling(attempt)
    };
    let delay = jitter(ceiling, rng);
    if delay.is_zero() {
        return None;
    }
    chrono::Duration::from_std(delay).ok().map(|d| now + d)
}

/// Full jitter: uniform in `[0, ceiling]`, at millisecond resolution.
fn jitter(ceiling: Duration, rng: &mut impl Rng) -> Duration {
    let max = u64::try_from(ceiling.as_millis()).unwrap_or(u64::MAX);
    Duration::from_millis(rng.random_range(0..=max))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn rng() -> StdRng {
        StdRng::seed_from_u64(42)
    }

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    fn crash() -> Event {
        Event::Exited { exit_code: Some(1) }
    }

    /// The delay a decision asks for, or zero for `StartNow`.
    fn delay_of(decision: Decision) -> Duration {
        match decision {
            Decision::StartNow => Duration::ZERO,
            Decision::WaitUntil(at) => (at - now()).to_std().unwrap(),
            other => panic!("expected a retry, got {other:?}"),
        }
    }

    #[test]
    fn ceiling_doubles_from_base_then_caps() {
        let p = RestartPolicy::default();
        let ceilings: Vec<u64> = (1..=7).map(|n| p.delay_ceiling(n).as_secs()).collect();
        assert_eq!(ceilings, [10, 20, 40, 80, 160, 300, 300]);
    }

    #[test]
    fn ceiling_does_not_overflow_on_huge_attempts() {
        let p = RestartPolicy::default();
        assert_eq!(p.delay_ceiling(64), p.cap);
        assert_eq!(p.delay_ceiling(u32::MAX), p.cap);
    }

    #[test]
    fn delay_stays_within_the_jittered_ceiling() {
        let p = RestartPolicy::default();
        let mut rng = rng();
        for _ in 0..200 {
            let mut state = RestartState::default();
            for attempt in 1..=10 {
                let d = decide(
                    &p,
                    WorkloadKind::Worker,
                    &mut state,
                    crash(),
                    now(),
                    &mut rng,
                );
                assert!(delay_of(d) <= p.delay_ceiling(attempt));
            }
        }
    }

    #[test]
    fn jitter_actually_spreads_delays() {
        let p = RestartPolicy::default();
        let mut rng = rng();
        let delays: Vec<Duration> = (0..50)
            .map(|_| {
                let mut state = RestartState {
                    restart_count: 9,
                    ..Default::default()
                };
                delay_of(decide(
                    &p,
                    WorkloadKind::Worker,
                    &mut state,
                    crash(),
                    now(),
                    &mut rng,
                ))
            })
            .collect();
        let min = delays.iter().min().unwrap();
        let max = delays.iter().max().unwrap();
        assert!(
            *max - *min > secs(60),
            "delays not spread: {min:?}..{max:?}"
        );
    }

    #[test]
    fn worker_never_gives_up_by_default() {
        let p = RestartPolicy::default();
        let mut state = RestartState::default();
        let mut rng = rng();
        for _ in 0..1000 {
            let d = decide(
                &p,
                WorkloadKind::Worker,
                &mut state,
                crash(),
                now(),
                &mut rng,
            );
            assert_ne!(d, Decision::Fail);
        }
        assert_eq!(state.restart_count, 1000);
    }

    #[test]
    fn worker_exiting_zero_is_restarted_not_completed() {
        let p = RestartPolicy::default();
        let mut state = RestartState::default();
        let d = decide(
            &p,
            WorkloadKind::Worker,
            &mut state,
            Event::Exited { exit_code: Some(0) },
            now(),
            &mut rng(),
        );
        assert_ne!(d, Decision::Complete);
        assert_eq!(state.restart_count, 1);
    }

    #[test]
    fn worker_under_fail_gives_up_after_max_attempts() {
        let p = RestartPolicy {
            on_exhaustion: OnExhaustion::Fail,
            max_attempts: 3,
            ..Default::default()
        };
        let mut state = RestartState::default();
        let mut rng = rng();
        for _ in 0..3 {
            let d = decide(
                &p,
                WorkloadKind::Worker,
                &mut state,
                crash(),
                now(),
                &mut rng,
            );
            assert_ne!(d, Decision::Fail);
        }
        let d = decide(
            &p,
            WorkloadKind::Worker,
            &mut state,
            crash(),
            now(),
            &mut rng,
        );
        assert_eq!(d, Decision::Fail);
    }

    #[test]
    fn job_completes_on_exit_zero() {
        let p = RestartPolicy::default();
        let mut state = RestartState {
            restart_count: 2,
            ..Default::default()
        };
        let d = decide(
            &p,
            WorkloadKind::Job { backoff_limit: 6 },
            &mut state,
            Event::Exited { exit_code: Some(0) },
            now(),
            &mut rng(),
        );
        assert_eq!(d, Decision::Complete);
        assert_eq!(state.restart_count, 2);
    }

    #[test]
    fn job_fails_once_failures_exceed_backoff_limit() {
        let p = RestartPolicy::default();
        let kind = WorkloadKind::Job { backoff_limit: 2 };
        let mut state = RestartState::default();
        let mut rng = rng();
        assert_ne!(
            decide(&p, kind, &mut state, crash(), now(), &mut rng),
            Decision::Fail
        );
        assert_ne!(
            decide(&p, kind, &mut state, crash(), now(), &mut rng),
            Decision::Fail
        );
        assert_eq!(
            decide(&p, kind, &mut state, crash(), now(), &mut rng),
            Decision::Fail
        );
    }

    #[test]
    fn a_job_that_cannot_start_is_retried_without_limit() {
        let p = RestartPolicy::default();
        let kind = WorkloadKind::Job { backoff_limit: 0 };
        let mut state = RestartState::default();
        let mut rng = rng();
        for _ in 0..20 {
            let d = decide(
                &p,
                kind,
                &mut state,
                Event::StartFailed(StartFailure::Transient),
                now(),
                &mut rng,
            );
            assert_ne!(d, Decision::Fail);
        }
        assert_eq!(state.run_failures, 0);
        assert_eq!(
            state.restart_count, 20,
            "start failures still space the retries"
        );
    }

    #[test]
    fn start_failures_do_not_count_against_backoff_limit() {
        let p = RestartPolicy::default();
        let kind = WorkloadKind::Job { backoff_limit: 1 };
        let mut state = RestartState::default();
        let mut rng = rng();
        for _ in 0..3 {
            decide(
                &p,
                kind,
                &mut state,
                Event::StartFailed(StartFailure::Transient),
                now(),
                &mut rng,
            );
        }
        assert_ne!(
            decide(&p, kind, &mut state, crash(), now(), &mut rng),
            Decision::Fail
        );
        assert_eq!(
            decide(&p, kind, &mut state, crash(), now(), &mut rng),
            Decision::Fail
        );
    }

    #[test]
    fn a_failed_job_is_not_run_again_by_default() {
        let p = RestartPolicy::default();
        let mut state = RestartState::default();
        let d = decide(
            &p,
            WorkloadKind::Job {
                backoff_limit: DEFAULT_BACKOFF_LIMIT,
            },
            &mut state,
            crash(),
            now(),
            &mut rng(),
        );
        assert_eq!(d, Decision::Fail);
    }

    #[test]
    fn job_with_zero_backoff_limit_fails_on_first_failure() {
        let p = RestartPolicy::default();
        let mut state = RestartState::default();
        let d = decide(
            &p,
            WorkloadKind::Job { backoff_limit: 0 },
            &mut state,
            crash(),
            now(),
            &mut rng(),
        );
        assert_eq!(d, Decision::Fail);
    }

    #[test]
    fn job_with_unknown_exit_code_is_a_failure() {
        let p = RestartPolicy::default();
        let mut state = RestartState::default();
        let d = decide(
            &p,
            WorkloadKind::Job { backoff_limit: 0 },
            &mut state,
            Event::Exited { exit_code: None },
            now(),
            &mut rng(),
        );
        assert_eq!(d, Decision::Fail);
    }

    #[test]
    fn start_failures_count_as_attempts() {
        let p = RestartPolicy::default();
        let mut state = RestartState::default();
        decide(
            &p,
            WorkloadKind::Worker,
            &mut state,
            Event::StartFailed(StartFailure::Transient),
            now(),
            &mut rng(),
        );
        assert_eq!(state.restart_count, 1);
    }

    #[test]
    fn operator_errors_start_at_the_cap() {
        let p = RestartPolicy::default();
        let mut rng = rng();
        let delays: Vec<Duration> = (0..50)
            .map(|_| {
                let mut state = RestartState::default();
                delay_of(decide(
                    &p,
                    WorkloadKind::Worker,
                    &mut state,
                    Event::StartFailed(StartFailure::NeedsOperator),
                    now(),
                    &mut rng,
                ))
            })
            .collect();
        // First attempt of an ordinary failure is bounded by `base` (10s);
        // operator errors draw from the whole `[0, cap]` range instead.
        assert!(delays.iter().all(|d| *d <= p.cap));
        assert!(delays.iter().any(|d| *d > p.base));
    }

    #[test]
    fn unrunnable_program_exits_start_at_the_cap() {
        let p = RestartPolicy::default();
        let mut rng = rng();
        let delays: Vec<Duration> = (0..50)
            .map(|_| {
                let mut state = RestartState::default();
                delay_of(decide(
                    &p,
                    WorkloadKind::Worker,
                    &mut state,
                    Event::Exited {
                        exit_code: Some(127),
                    },
                    now(),
                    &mut rng,
                ))
            })
            .collect();
        assert!(delays.iter().any(|d| *d > p.base));
    }

    #[test]
    fn counter_resets_after_stable_after_of_running() {
        let p = RestartPolicy::default();
        let mut state = RestartState {
            restart_count: 7,
            ..Default::default()
        };
        on_running(&mut state, now());
        assert!(!reset_if_stable(
            &p,
            &mut state,
            now() + chrono::Duration::minutes(9)
        ));
        assert_eq!(state.restart_count, 7);
        assert!(reset_if_stable(
            &p,
            &mut state,
            now() + chrono::Duration::minutes(10)
        ));
        assert_eq!(state.restart_count, 0);
    }

    #[test]
    fn short_runs_do_not_reset_the_counter() {
        // A worker crashing every 15s must keep climbing the backoff curve.
        let p = RestartPolicy::default();
        let mut state = RestartState::default();
        let mut rng = rng();
        let mut t = now();
        for _ in 0..5 {
            on_running(&mut state, t);
            t += chrono::Duration::seconds(15);
            decide(&p, WorkloadKind::Worker, &mut state, crash(), t, &mut rng);
        }
        assert_eq!(state.restart_count, 5);
    }

    #[test]
    fn crash_after_a_long_run_starts_from_the_first_attempt() {
        let p = RestartPolicy::default();
        let mut state = RestartState {
            restart_count: 9,
            ..Default::default()
        };
        on_running(&mut state, now());
        let later = now() + chrono::Duration::hours(1);
        decide(
            &p,
            WorkloadKind::Worker,
            &mut state,
            crash(),
            later,
            &mut rng(),
        );
        assert_eq!(state.restart_count, 1);
        assert_eq!(state.running_since, None);
    }

    #[test]
    fn on_running_keeps_the_start_of_an_ongoing_run() {
        let mut state = RestartState::default();
        on_running(&mut state, now());
        on_running(&mut state, now() + chrono::Duration::minutes(5));
        assert_eq!(state.running_since, Some(now()));
    }

    #[test]
    fn default_policy_is_valid() {
        assert_eq!(RestartPolicy::default().validate(), Ok(()));
    }

    #[test]
    fn override_replaces_only_the_keys_it_sets() {
        let server = RestartPolicy::default();
        let resolved = server
            .with_override(&RestartOverride {
                cap: Some(secs(30)),
                on_exhaustion: Some(OnExhaustion::Fail),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            resolved,
            RestartPolicy {
                cap: secs(30),
                on_exhaustion: OnExhaustion::Fail,
                ..server
            }
        );
    }

    #[test]
    fn empty_override_is_the_server_policy() {
        let server = RestartPolicy::default();
        assert_eq!(
            server.with_override(&RestartOverride::default()),
            Ok(server)
        );
    }

    #[test]
    fn override_is_validated_against_the_server_values() {
        // cap alone, below the server's base.
        let err = RestartPolicy::default()
            .with_override(&RestartOverride {
                cap: Some(secs(5)),
                ..Default::default()
            })
            .unwrap_err();
        assert_eq!(
            err,
            PolicyError::CapBelowBase {
                base: secs(10),
                cap: secs(5)
            }
        );
    }

    #[test]
    fn invalid_policies_are_rejected() {
        let p = RestartPolicy::default();
        let cases = [
            (
                RestartOverride {
                    base: Some(Duration::ZERO),
                    ..Default::default()
                },
                PolicyError::ZeroBase,
            ),
            (
                RestartOverride {
                    stable_after: Some(Duration::ZERO),
                    ..Default::default()
                },
                PolicyError::ZeroStableAfter,
            ),
            (
                RestartOverride {
                    max_attempts: Some(0),
                    ..Default::default()
                },
                PolicyError::ZeroMaxAttempts,
            ),
        ];
        for (o, expected) in cases {
            assert_eq!(p.with_override(&o), Err(expected));
        }
    }
}
