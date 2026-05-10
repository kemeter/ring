use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::time::Duration;

fn default_threshold() -> u32 {
    3
}

fn default_readiness() -> bool {
    false
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type")]
pub(crate) enum HealthCheck {
    #[serde(rename = "tcp")]
    Tcp {
        port: u16,
        interval: String,
        timeout: String,
        #[serde(default = "default_threshold")]
        threshold: u32,
        on_failure: FailureAction,
        /// When true, the rolling-update scheduler waits for this check to
        /// pass before draining the parent. Type-agnostic: works for tcp,
        /// http and command. Only `command` readiness checks are also
        /// pushed to Docker as a native `HEALTHCHECK` so the proxy
        /// (Traefik / Sozune) can gate traffic — tcp/http have no native
        /// Docker equivalent and are documented as Ring-only readiness.
        #[serde(default = "default_readiness")]
        readiness: bool,
    },
    #[serde(rename = "http")]
    Http {
        url: String,
        interval: String,
        timeout: String,
        #[serde(default = "default_threshold")]
        threshold: u32,
        on_failure: FailureAction,
        #[serde(default = "default_readiness")]
        readiness: bool,
    },
    #[serde(rename = "command")]
    Command {
        command: String,
        interval: String,
        timeout: String,
        #[serde(default = "default_threshold")]
        threshold: u32,
        on_failure: FailureAction,
        #[serde(default = "default_readiness")]
        readiness: bool,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "lowercase")]
pub(crate) enum FailureAction {
    Restart,
    Stop,
    Alert,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub(crate) struct HealthCheckResult {
    pub(crate) id: String,
    pub(crate) deployment_id: String,
    pub(crate) check_type: String,
    pub(crate) status: HealthCheckStatus,
    pub(crate) message: Option<String>,
    pub(crate) created_at: String,
    pub(crate) started_at: String,
    pub(crate) finished_at: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "lowercase")]
pub(crate) enum HealthCheckStatus {
    Success,
    Failed,
    Timeout,
}

impl HealthCheck {
    pub(crate) fn parse_duration(duration_str: &str) -> Result<Duration, String> {
        if let Some(stripped) = duration_str.strip_suffix("ms") {
            let millis = stripped
                .parse::<u64>()
                .map_err(|_| format!("Invalid duration format: {}", duration_str))?;
            Ok(Duration::from_millis(millis))
        } else if let Some(stripped) = duration_str.strip_suffix('s') {
            let seconds = stripped
                .parse::<u64>()
                .map_err(|_| format!("Invalid duration format: {}", duration_str))?;
            Ok(Duration::from_secs(seconds))
        } else {
            Err(format!("Invalid duration format: {}", duration_str))
        }
    }

    pub(crate) fn timeout(&self) -> &str {
        match self {
            HealthCheck::Tcp { timeout, .. } => timeout,
            HealthCheck::Http { timeout, .. } => timeout,
            HealthCheck::Command { timeout, .. } => timeout,
        }
    }

    pub(crate) fn threshold(&self) -> u32 {
        match self {
            HealthCheck::Tcp { threshold, .. } => *threshold,
            HealthCheck::Http { threshold, .. } => *threshold,
            HealthCheck::Command { threshold, .. } => *threshold,
        }
    }

    pub(crate) fn on_failure(&self) -> &FailureAction {
        match self {
            HealthCheck::Tcp { on_failure, .. } => on_failure,
            HealthCheck::Http { on_failure, .. } => on_failure,
            HealthCheck::Command { on_failure, .. } => on_failure,
        }
    }

    pub(crate) fn check_type(&self) -> &str {
        match self {
            HealthCheck::Tcp { .. } => "tcp",
            HealthCheck::Http { .. } => "http",
            HealthCheck::Command { .. } => "command",
        }
    }

    pub(crate) fn is_readiness(&self) -> bool {
        match self {
            HealthCheck::Tcp { readiness, .. } => *readiness,
            HealthCheck::Http { readiness, .. } => *readiness,
            HealthCheck::Command { readiness, .. } => *readiness,
        }
    }

    pub(crate) fn interval(&self) -> &str {
        match self {
            HealthCheck::Tcp { interval, .. } => interval,
            HealthCheck::Http { interval, .. } => interval,
            HealthCheck::Command { interval, .. } => interval,
        }
    }
}

impl Default for HealthCheck {
    fn default() -> Self {
        HealthCheck::Tcp {
            port: 8080,
            interval: "30s".to_string(),
            timeout: "5s".to_string(),
            threshold: 3,
            on_failure: FailureAction::Restart,
            readiness: false,
        }
    }
}

/// Decision from `evaluate_readiness`. The variants spell out exactly *why*
/// the rolling update can or can't progress so callers can log a precise event
/// instead of a generic "not ready".
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ReadinessDecision {
    /// No readiness HC is configured on the deployment — the caller should
    /// fall back to the legacy "drain on Running" behaviour. This is what
    /// keeps the new flag opt-in for existing deployments.
    NotConfigured,
    /// At least one readiness HC has never produced a result yet.
    PendingNoResult,
    /// All readiness HCs are currently passing, but the most recent success
    /// happened too recently — keep waiting until `min_healthy_time` elapses
    /// to filter out flapping checks.
    PendingMinHealthyTime { remaining: Duration },
    /// At least one readiness HC is currently failing.
    Failing,
    /// All readiness HCs have been Success for at least `min_healthy_time` —
    /// the rolling update may drain the parent.
    Ready,
}

/// Decide whether a deployment with these readiness checks is ready to drain
/// its rolling-update parent.
///
/// Pure function so the test suite can exhaustively exercise the decision
/// table without spinning up a database or sleeping. The caller passes:
/// - `expected_readiness_count`: how many HCs in the deployment manifest are
///   marked `readiness: true`. Used to detect "I'm waiting on a HC that
///   hasn't produced any result yet".
/// - `latest_readiness_results`: the freshest result for each readiness HC
///   that has produced at least one entry (one row per `check_type`).
/// - `min_healthy_time`: anti-flap window, injected (not hardcoded) so tests
///   can pass `0s` and skip it deterministically.
pub(crate) fn evaluate_readiness(
    expected_readiness_count: usize,
    latest_readiness_results: &[(HealthCheckStatus, DateTime<Utc>)],
    now: DateTime<Utc>,
    min_healthy_time: Duration,
) -> ReadinessDecision {
    if expected_readiness_count == 0 {
        return ReadinessDecision::NotConfigured;
    }

    if latest_readiness_results.len() < expected_readiness_count {
        return ReadinessDecision::PendingNoResult;
    }

    // Any failing readiness check vetoes the drain immediately. We don't
    // distinguish Failed from Timeout here — both mean "not ready". The
    // operator who wants more nuance can read the underlying logs.
    if latest_readiness_results
        .iter()
        .any(|(s, _)| !matches!(s, HealthCheckStatus::Success))
    {
        return ReadinessDecision::Failing;
    }

    // Find the freshest finished_at among the readiness checks. The drain
    // is gated by *the latest* one — if any single readiness HC just turned
    // Success a moment ago, we still need to ride out the anti-flap window.
    let freshest = latest_readiness_results
        .iter()
        .map(|(_, finished_at)| *finished_at)
        .max()
        .expect("non-empty latest_readiness_results checked above");

    let elapsed = now.signed_duration_since(freshest);
    let elapsed_std = match elapsed.to_std() {
        Ok(d) => d,
        // Negative elapsed means the latest finished_at is in the future
        // (clock skew or test fixture). Treat as "still fresh".
        Err(_) => {
            return ReadinessDecision::PendingMinHealthyTime {
                remaining: min_healthy_time,
            };
        }
    };

    if elapsed_std >= min_healthy_time {
        ReadinessDecision::Ready
    } else {
        ReadinessDecision::PendingMinHealthyTime {
            remaining: min_healthy_time - elapsed_std,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_duration_seconds() {
        let result = HealthCheck::parse_duration("30s");
        assert!(result.is_ok());
        assert_eq!(result.unwrap().as_secs(), 30);
    }

    #[test]
    fn test_parse_duration_milliseconds() {
        let result = HealthCheck::parse_duration("500ms");
        assert!(result.is_ok());
        assert_eq!(result.unwrap().as_millis(), 500);
    }

    #[test]
    fn test_parse_duration_invalid() {
        let result = HealthCheck::parse_duration("30");
        assert!(result.is_err());

        let result = HealthCheck::parse_duration("abc");
        assert!(result.is_err());
    }

    fn fixed_now() -> DateTime<Utc> {
        // Arbitrary fixed timestamp so tests are deterministic. The exact
        // value doesn't matter — only its delta to the result timestamps does.
        DateTime::parse_from_rfc3339("2026-05-10T19:42:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn ts(seconds_ago: i64) -> DateTime<Utc> {
        fixed_now() - chrono::Duration::seconds(seconds_ago)
    }

    #[test]
    fn readiness_not_configured_when_no_readiness_checks() {
        let decision = evaluate_readiness(0, &[], fixed_now(), Duration::from_secs(10));
        assert_eq!(decision, ReadinessDecision::NotConfigured);
    }

    #[test]
    fn readiness_pending_when_expected_check_has_no_result_yet() {
        // Two readiness HCs declared, only one has produced a result so far.
        let results = [(HealthCheckStatus::Success, ts(20))];
        let decision = evaluate_readiness(2, &results, fixed_now(), Duration::from_secs(10));
        assert_eq!(decision, ReadinessDecision::PendingNoResult);
    }

    #[test]
    fn readiness_failing_when_any_latest_is_failed() {
        let results = [
            (HealthCheckStatus::Success, ts(20)),
            (HealthCheckStatus::Failed, ts(2)),
        ];
        let decision = evaluate_readiness(2, &results, fixed_now(), Duration::from_secs(10));
        assert_eq!(decision, ReadinessDecision::Failing);
    }

    #[test]
    fn readiness_failing_when_any_latest_is_timeout() {
        let results = [
            (HealthCheckStatus::Success, ts(20)),
            (HealthCheckStatus::Timeout, ts(1)),
        ];
        let decision = evaluate_readiness(2, &results, fixed_now(), Duration::from_secs(10));
        assert_eq!(decision, ReadinessDecision::Failing);
    }

    #[test]
    fn readiness_pending_min_healthy_when_freshest_is_too_recent() {
        // Both Success but the freshest finished 5s ago — anti-flap window
        // is 10s, so we must wait 5 more seconds.
        let results = [
            (HealthCheckStatus::Success, ts(20)),
            (HealthCheckStatus::Success, ts(5)),
        ];
        let decision = evaluate_readiness(2, &results, fixed_now(), Duration::from_secs(10));
        match decision {
            ReadinessDecision::PendingMinHealthyTime { remaining } => {
                assert_eq!(remaining, Duration::from_secs(5));
            }
            other => panic!("expected PendingMinHealthyTime, got {:?}", other),
        }
    }

    #[test]
    fn readiness_ready_when_freshest_is_old_enough() {
        let results = [
            (HealthCheckStatus::Success, ts(30)),
            (HealthCheckStatus::Success, ts(15)),
        ];
        let decision = evaluate_readiness(2, &results, fixed_now(), Duration::from_secs(10));
        assert_eq!(decision, ReadinessDecision::Ready);
    }

    #[test]
    fn readiness_ready_immediately_when_min_healthy_time_is_zero() {
        // Useful as a deterministic test mode and as the "no anti-flap"
        // configuration if an operator ever opts out.
        let results = [(HealthCheckStatus::Success, ts(0))];
        let decision = evaluate_readiness(1, &results, fixed_now(), Duration::from_secs(0));
        assert_eq!(decision, ReadinessDecision::Ready);
    }

    #[test]
    fn readiness_handles_future_finished_at_gracefully() {
        // Clock skew between the result writer and the gate evaluator can
        // produce a finished_at slightly in the future. Treat it as "still
        // fresh" rather than panicking.
        let results = [(HealthCheckStatus::Success, ts(-5))];
        let decision = evaluate_readiness(1, &results, fixed_now(), Duration::from_secs(10));
        match decision {
            ReadinessDecision::PendingMinHealthyTime { remaining } => {
                assert_eq!(remaining, Duration::from_secs(10));
            }
            other => panic!("expected PendingMinHealthyTime, got {:?}", other),
        }
    }
}
