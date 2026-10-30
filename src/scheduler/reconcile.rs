//! Reconciliation of deployments on runtimes that implement [`InstanceDriver`].
//!
//! The runtime reports which instances run and which ended; this module decides
//! what to do about it with the restart policy, and asks the runtime to start
//! or stop instances accordingly. It is the same code for every such runtime,
//! so a runtime holds no restart counter, no backoff and no status logic.
//!
//! - An ended instance is a failure, whatever its exit code: a worker is a
//!   service, and a service that exits must come back. Each failure is counted
//!   and pushes the next start out on the backoff curve.
//! - While backing off, nothing is started. Scaling down still happens.
//! - Once the instances have run for `stable_after` without a failure, the
//!   counter resets.
//! - A worker never gives up unless the policy says `on_exhaustion = "fail"`.
//! - A job runs one instance. It completes on exit 0 and fails once its runs
//!   that exited non-zero exceed `backoff_limit`; the instance of a finished job
//!   is [retired](InstanceDriver::retire_instance). A job that cannot start is
//!   retried without limit.

use crate::hypervisor::classifier::{
    Disposition, classify_create_error, classify_exit_code, create_error_outcome,
};
use crate::hypervisor::instance_driver::InstanceDriver;
use crate::models::deployments::{Deployment, DeploymentStatus, Reason};
use crate::models::restart_state::RestartState;
use crate::models::volume::ResolvedMount;
use crate::scheduler::restart::{self, Decision, Event, RestartPolicy, StartFailure, WorkloadKind};
use chrono::{DateTime, Utc};
use rand::Rng;

/// One reconciliation pass. Returns the deployment with its new status,
/// instances, `restart_count` and pending events, and updates `state`.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn reconcile(
    driver: &dyn InstanceDriver,
    policy: &RestartPolicy,
    kind: WorkloadKind,
    mut deployment: Deployment,
    resolved_mounts: &[ResolvedMount],
    state: &mut RestartState,
    now: DateTime<Utc>,
    rng: &mut (impl Rng + Send),
) -> Deployment {
    let mut counter = restart::RestartState {
        restart_count: deployment.restart_count,
        running_since: state.running_since,
        run_failures: state.run_failures,
    };

    let observation = match driver.observe(&deployment).await {
        Ok(observation) => observation,
        Err(e) => {
            // Starting anything now could double an instance it did not see.
            warn!(
                "Deployment {}: could not observe its instances, skipping this pass: {}",
                deployment.id, e
            );
            return deployment;
        }
    };
    deployment.instances = observation.running;
    // Running when this pass started: the deployment already got past the
    // readiness gate. Promoted below, it may still be held back by the gate.
    let was_running = deployment.status == DeploymentStatus::Running;
    // An instance started on an earlier pass and still running now has proven
    // it stays up: that, not the start itself, is what promotes a progressing
    // deployment. The readiness gate then has its say on this promotion.
    if deployment.status == DeploymentStatus::Progressing && !deployment.instances.is_empty() {
        deployment.set_status(DeploymentStatus::Running);
    }

    // A run that never got past the readiness gate is not the stable run that
    // earns the budget back, however long it lasted: its end is counted
    // without looking at its length. Kept otherwise, for the readiness deadline.
    if !was_running && !observation.terminated.is_empty() {
        counter.running_since = None;
    }
    // Why the last instance that ended this pass failed, and how.
    let mut failure: Option<(Reason, String)> = None;
    for termination in observation.terminated {
        if !termination.started {
            // Its start was refused and the leftover could not be cleaned up
            // at the time: a failed start, whatever exit code it reports.
            let decision = restart::decide(
                policy,
                kind,
                &mut counter,
                Event::StartFailed(StartFailure::Transient),
                now,
                rng,
            );
            let message = format!(
                "Instance {} never started",
                short_id(&termination.instance_id)
            );
            deployment.emit_event(
                "error",
                format!("{message}; removing it"),
                &deployment.runtime.clone(),
                Some("instance_never_started"),
            );
            driver.discard_instance(&termination.instance_id).await;
            let reason = Reason::CrashLoop;
            apply_decision(
                &mut deployment,
                state,
                decision,
                counter.restart_count,
                reason,
                &message,
            );
            keep_worst(&mut failure, reason, message);
            continue;
        }
        let decision = restart::decide(
            policy,
            kind,
            &mut counter,
            Event::Exited {
                exit_code: termination.exit_code,
            },
            now,
            rng,
        );
        if decision == Decision::Complete {
            deployment.set_status(DeploymentStatus::Completed);
            deployment.emit_event(
                "info",
                "Job completed".to_string(),
                "scheduler",
                Some("job_completed"),
            );
            driver.retire_instance(&termination.instance_id).await;
            state.last_termination = Some(termination);
            state.next_attempt_at = None;
            break;
        }
        let message = format!(
            "Instance {} exited with code {}",
            short_id(&termination.instance_id),
            termination
                .exit_code
                .map_or_else(|| "unknown".to_string(), |c| c.to_string()),
        );
        deployment.emit_event(
            "error",
            format!("{message} (restart {})", counter.restart_count),
            &deployment.runtime.clone(),
            Some("container_crashed"),
        );
        let mut reason = ended_reason(kind);
        if let Disposition::Terminal(terminal) = classify_exit_code(termination.exit_code) {
            deployment.emit_event(
                "error",
                format!(
                    "Instance {} cannot run its program (exit code {:?}); fix the image or command",
                    short_id(&termination.instance_id),
                    termination.exit_code
                ),
                &deployment.runtime.clone(),
                Some("non_retryable_exit"),
            );
            reason = terminal;
        }
        let job_done = matches!(kind, WorkloadKind::Job { .. }) && decision == Decision::Fail;
        if job_done {
            driver.retire_instance(&termination.instance_id).await;
        } else {
            driver.discard_instance(&termination.instance_id).await;
        }
        state.last_termination = Some(termination);
        apply_decision(
            &mut deployment,
            state,
            decision,
            counter.restart_count,
            reason,
            &message,
        );
        keep_worst(&mut failure, reason, message);
    }

    if matches!(
        deployment.status,
        DeploymentStatus::Failed | DeploymentStatus::Completed
    ) {
        deployment.restart_count = counter.restart_count;
        state.run_failures = counter.run_failures;
        state.running_since = None;
        return deployment;
    }
    // With nothing left running, the deployment waits for its next start. A
    // program that cannot run says so even while other instances still do.
    let failed_this_pass = failure.is_some();
    if let Some((reason, message)) = failure
        && (deployment.instances.is_empty() || reason == Reason::SpecRejected)
    {
        deployment.back_off(reason, message);
    }

    scale(
        driver,
        policy,
        kind,
        &mut deployment,
        resolved_mounts,
        state,
        &mut counter,
        now,
        rng,
    )
    .await;

    // Backing off with instances still serving (a program that cannot run in
    // one of them) and nothing left to start, once the target came down to the
    // instances left: no start will take it out of backing_off, so it goes
    // back through progressing and the readiness gate like after a start.
    if deployment.status == DeploymentStatus::BackingOff
        && !failed_this_pass
        && kind == WorkloadKind::Worker
        && !deployment.instances.is_empty()
        && deployment.instances.len() as u64 >= u64::from(deployment.target_replicas())
    {
        deployment.set_status(DeploymentStatus::Progressing);
    }

    if deployment.instances.is_empty() {
        counter.running_since = None;
    } else if !failed_this_pass && kind == WorkloadKind::Worker {
        restart::on_running(&mut counter, now);
        // Only a deployment that is ready earns its budget back: one held by
        // the readiness gate stays up without serving, and resetting it there
        // would make each readiness timeout look like a first failure.
        if was_running && restart::reset_if_stable(policy, &mut counter, now) {
            deployment.emit_event(
                "info",
                format!(
                    "Restart count reset after {} of uninterrupted running",
                    humanize(policy.stable_after)
                ),
                "scheduler",
                Some("restart_count_reset"),
            );
        }
    }

    deployment.restart_count = counter.restart_count;
    state.running_since = counter.running_since;
    state.run_failures = counter.run_failures;
    deployment
}

/// Count instances the scheduler removed because they failed a check (a
/// liveness check, the readiness deadline) as failures, exactly like instances
/// that exited on their own. For a job that is a failed run, bounded by its
/// `backoff_limit`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn record_removals(
    policy: &RestartPolicy,
    kind: WorkloadKind,
    deployment: &mut Deployment,
    state: &mut RestartState,
    removed: usize,
    reason: Reason,
    message: &str,
    now: DateTime<Utc>,
    rng: &mut impl Rng,
) {
    let mut counter = restart::RestartState {
        restart_count: deployment.restart_count,
        running_since: state.running_since,
        run_failures: state.run_failures,
    };
    for _ in 0..removed {
        let decision = restart::decide(
            policy,
            kind,
            &mut counter,
            Event::Exited { exit_code: None },
            now,
            rng,
        );
        apply_decision(
            deployment,
            state,
            decision,
            counter.restart_count,
            reason,
            message,
        );
    }
    deployment.restart_count = counter.restart_count;
    state.running_since = counter.running_since;
    state.run_failures = counter.run_failures;
    // The last instance was removed: the deployment is waiting for its next
    // start like after a crash, not running.
    if removed > 0
        && deployment.instances.is_empty()
        && deployment.status != DeploymentStatus::Failed
    {
        deployment.back_off(reason, message);
    }
}

/// Count a readiness timeout, once its instances are gone: one failed attempt.
/// The run lasted the whole deadline without ever being ready, which is not
/// the stable run that earns the budget back, so its length is not looked at.
pub(crate) fn record_readiness_timeout(
    policy: &RestartPolicy,
    deployment: &mut Deployment,
    state: &mut RestartState,
    message: &str,
    now: DateTime<Utc>,
    rng: &mut impl Rng,
) {
    state.running_since = None;
    let kind = deployment.workload_kind();
    record_removals(
        policy,
        kind,
        deployment,
        state,
        1,
        Reason::ReadinessTimeout,
        message,
        now,
        rng,
    );
}

/// Record why an instance failed this pass, keeping a program that cannot run
/// over a later ordinary crash: it is the one an operator has to fix.
fn keep_worst(failure: &mut Option<(Reason, String)>, reason: Reason, message: String) {
    if !matches!(failure, Some((Reason::SpecRejected, _))) {
        *failure = Some((reason, message));
    }
}

/// Why an instance that ended on its own failed: a worker must not end, and a
/// job run that ends badly is a failed run.
pub(crate) fn ended_reason(kind: WorkloadKind) -> Reason {
    match kind {
        WorkloadKind::Worker => Reason::CrashLoop,
        WorkloadKind::Job { .. } => Reason::RunFailed,
    }
}

#[allow(clippy::too_many_arguments)]
async fn scale(
    driver: &dyn InstanceDriver,
    policy: &RestartPolicy,
    kind: WorkloadKind,
    deployment: &mut Deployment,
    resolved_mounts: &[ResolvedMount],
    state: &mut RestartState,
    counter: &mut restart::RestartState,
    now: DateTime<Utc>,
    rng: &mut (impl Rng + Send),
) {
    let current = deployment.instances.len();
    // A job runs exactly one instance, whatever `replicas` says, and a running
    // one is never stopped to scale down.
    let target = match kind {
        WorkloadKind::Job { .. } => 1,
        WorkloadKind::Worker => usize::try_from(deployment.target_replicas()).unwrap_or(usize::MAX),
    };

    if current > target && kind == WorkloadKind::Worker {
        let Some(instance_id) = deployment.instances.first().cloned() else {
            return;
        };
        if driver.stop_instance(&instance_id).await {
            deployment.instances.remove(0);
            deployment.emit_event(
                "info",
                format!(
                    "Scaled down from {} to {} replicas (removed instance {})",
                    current,
                    current - 1,
                    short_id(&instance_id)
                ),
                &deployment.runtime.clone(),
                Some("scale_down"),
            );
        }
        return;
    }

    if current >= target {
        return;
    }

    if state.backing_off(now) {
        debug!(
            "Deployment {} backing off until {:?}",
            deployment.id, state.next_attempt_at
        );
        return;
    }

    match driver.start_instance(deployment, resolved_mounts).await {
        Ok(()) => {
            state.next_attempt_at = None;
            deployment.emit_event(
                "info",
                format!("Scaled up from {} to {} replicas", current, current + 1),
                &deployment.runtime.clone(),
                Some("scale_up"),
            );
            // Progressing until the next pass sees the instance still running.
            // Promoted now, a container that dies on start would show running
            // for a pass, and a worker restarted with nothing live would reach
            // running without going through the readiness gate, which only
            // holds a deployment that was progressing. A worker that still has
            // other live instances stays running.
            if deployment.status != DeploymentStatus::Running || current == 0 {
                deployment.set_status(DeploymentStatus::Progressing);
            }
        }
        Err(err) => {
            let (reason, event_reason, message) = create_error_outcome(&err, deployment);
            error!("[{}] {}: {}", deployment.id, event_reason, err);
            deployment.emit_event(
                "error",
                message.clone(),
                &deployment.runtime.clone(),
                Some(event_reason),
            );
            // Instances still serving keep the deployment as it was, their
            // health checks with it: only the next start waits. With none,
            // the deployment waits for it. So does a rolling update's new
            // deployment, which would otherwise drain the one it replaces
            // toward fewer instances than it can start. One already backing
            // off says why it last failed.
            if deployment.instances.is_empty()
                || deployment.parent_id.is_some()
                || deployment.status == DeploymentStatus::BackingOff
            {
                deployment.back_off(reason, message.clone());
            }

            let failure = match classify_create_error(&err) {
                Disposition::Terminal(
                    Reason::ConfigMissing | Reason::SpecRejected | Reason::Error,
                ) => StartFailure::NeedsOperator,
                _ => StartFailure::Transient,
            };
            let decision =
                restart::decide(policy, kind, counter, Event::StartFailed(failure), now, rng);
            apply_decision(
                deployment,
                state,
                decision,
                counter.restart_count,
                reason,
                &message,
            );
        }
    }
}

/// Apply the policy's decision on a failure for `reason`, described by
/// `message`.
fn apply_decision(
    deployment: &mut Deployment,
    state: &mut RestartState,
    decision: Decision,
    attempts: u32,
    reason: Reason,
    message: &str,
) {
    match decision {
        Decision::StartNow | Decision::Complete => state.next_attempt_at = None,
        Decision::WaitUntil(at) => state.next_attempt_at = Some(at),
        Decision::Fail => {
            state.next_attempt_at = None;
            if deployment.status != DeploymentStatus::Failed {
                deployment.fail(reason, message);
                deployment.emit_event(
                    "error",
                    format!("Giving up after {} failed attempts", attempts),
                    "scheduler",
                    Some("restart_limit_reached"),
                );
            }
        }
    }
}

fn short_id(id: &str) -> &str {
    &id[..id.len().min(12)]
}

fn humanize(d: std::time::Duration) -> String {
    let secs = d.as_secs();
    if secs.is_multiple_of(60) && secs >= 60 {
        format!("{}m", secs / 60)
    } else {
        format!("{}s", secs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hypervisor::error::RuntimeError;
    use crate::hypervisor::instance_driver::Observation;
    use crate::models::restart_state::Termination;
    use crate::scheduler::restart::OnExhaustion;
    use async_trait::async_trait;
    use rand::SeedableRng;
    use rand::rngs::StdRng;
    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::time::Duration;

    /// A runtime whose instances the test drives by hand.
    #[derive(Default)]
    struct FakeDriver {
        running: Mutex<Vec<String>>,
        terminated: Mutex<Vec<Termination>>,
        start_error: Mutex<Option<RuntimeError>>,
        started: Mutex<u32>,
        discarded: Mutex<Vec<String>>,
        retired: Mutex<Vec<String>>,
        /// `observe` fails, as with a daemon that cannot be reached.
        blind: Mutex<bool>,
        stopped: Mutex<Vec<String>>,
    }

    impl FakeDriver {
        fn running(ids: &[&str]) -> Self {
            let driver = Self::default();
            *driver.running.lock().unwrap() = ids.iter().map(|s| s.to_string()).collect();
            driver
        }

        /// The instance `id` exits with `code`.
        fn crash(&self, id: &str, code: Option<i64>) {
            self.running.lock().unwrap().retain(|r| r != id);
            self.terminated.lock().unwrap().push(Termination {
                instance_id: id.to_string(),
                exit_code: code,
                finished_at: now(),
                started: true,
                logs_tail: Some("panic".to_string()),
            });
        }

        fn fail_starts_with(&self, err: RuntimeError) {
            *self.start_error.lock().unwrap() = Some(err);
        }

        fn starts(&self) -> u32 {
            *self.started.lock().unwrap()
        }
    }

    #[async_trait]
    impl InstanceDriver for FakeDriver {
        async fn observe(&self, _deployment: &Deployment) -> Result<Observation, RuntimeError> {
            if *self.blind.lock().unwrap() {
                return Err(RuntimeError::Other("daemon unreachable".to_string()));
            }
            Ok(Observation {
                running: self.running.lock().unwrap().clone(),
                terminated: self.terminated.lock().unwrap().clone(),
            })
        }

        async fn start_instance(
            &self,
            deployment: &mut Deployment,
            _resolved_mounts: &[ResolvedMount],
        ) -> Result<(), RuntimeError> {
            if let Some(err) = self.start_error.lock().unwrap().take() {
                return Err(err);
            }
            let mut started = self.started.lock().unwrap();
            *started += 1;
            let id = format!("new-{started}");
            self.running.lock().unwrap().push(id.clone());
            deployment.instances.push(id);
            Ok(())
        }

        async fn stop_instance(&self, instance_id: &str) -> bool {
            self.running.lock().unwrap().retain(|r| r != instance_id);
            self.stopped.lock().unwrap().push(instance_id.to_string());
            true
        }

        async fn discard_instance(&self, instance_id: &str) {
            self.terminated
                .lock()
                .unwrap()
                .retain(|t| t.instance_id != instance_id);
            self.discarded.lock().unwrap().push(instance_id.to_string());
        }

        async fn retire_instance(&self, instance_id: &str) {
            self.retired.lock().unwrap().push(instance_id.to_string());
        }

        async fn teardown(&self, deployment: &mut Deployment) -> Result<(), RuntimeError> {
            deployment.instances.clear();
            Ok(())
        }
    }

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn worker(replicas: u32) -> Deployment {
        Deployment {
            id: "w1".to_string(),
            created_at: now().to_string(),
            updated_at: None,
            status: DeploymentStatus::Running,
            reason: None,
            message: None,
            restart_count: 0,
            namespace: "ns".to_string(),
            name: "web".to_string(),
            image: "nginx".to_string(),
            config: None,
            runtime: "docker".to_string(),
            kind: "worker".to_string(),
            replicas,
            command: vec![],
            instances: vec![],
            labels: HashMap::new(),
            environment: HashMap::new(),
            volumes: "[]".to_string(),
            health_checks: vec![],
            resources: None,
            autoscale: None,
            desired_replicas: None,
            image_digest: None,
            ports: vec![],
            pending_events: vec![],
            parent_id: None,
            network: None,
            restart: None,
        }
    }

    async fn pass(
        driver: &FakeDriver,
        policy: &RestartPolicy,
        deployment: Deployment,
        state: &mut RestartState,
        at: DateTime<Utc>,
    ) -> Deployment {
        let mut rng = StdRng::seed_from_u64(7);
        reconcile(
            driver,
            policy,
            WorkloadKind::Worker,
            deployment,
            &[],
            state,
            at,
            &mut rng,
        )
        .await
    }

    fn reasons(d: &Deployment) -> Vec<String> {
        d.pending_events
            .iter()
            .filter_map(|e| e.reason.clone())
            .collect()
    }

    fn secs(s: i64) -> chrono::Duration {
        chrono::Duration::seconds(s)
    }

    #[tokio::test]
    async fn nothing_is_started_when_the_instances_cannot_be_observed() {
        let driver = FakeDriver::running(&["a"]);
        *driver.blind.lock().unwrap() = true;
        let mut state = RestartState::default();

        let d = pass(
            &driver,
            &RestartPolicy::default(),
            worker(2),
            &mut state,
            now(),
        )
        .await;

        assert_eq!(driver.starts(), 0);
        assert_eq!(d.status, DeploymentStatus::Running);
        assert_eq!(d.restart_count, 0);
    }

    #[tokio::test]
    async fn a_crash_is_counted_recorded_and_discarded() {
        let driver = FakeDriver::running(&["a"]);
        driver.crash("a", Some(1));
        let mut state = RestartState::default();
        let policy = RestartPolicy::default();

        let d = pass(&driver, &policy, worker(1), &mut state, now()).await;

        assert_eq!(d.restart_count, 1);
        assert_eq!(*driver.discarded.lock().unwrap(), ["a"]);
        let last = state.last_termination.as_ref().unwrap();
        assert_eq!(last.exit_code, Some(1));
        assert_eq!(last.logs_tail.as_deref(), Some("panic"));
        assert!(reasons(&d).contains(&"container_crashed".to_string()));
        // First retry within `base`.
        if let Some(at) = state.next_attempt_at {
            assert!(at <= now() + secs(10));
        }
    }

    #[tokio::test]
    async fn nothing_starts_while_backing_off() {
        let driver = FakeDriver::default();
        let mut state = RestartState {
            next_attempt_at: Some(now() + secs(60)),
            ..Default::default()
        };
        let mut d = worker(1);
        d.back_off(Reason::CrashLoop, "test");
        d.restart_count = 3;

        let d = pass(&driver, &RestartPolicy::default(), d, &mut state, now()).await;

        assert_eq!(driver.starts(), 0);
        assert_eq!(
            d.condition(),
            (DeploymentStatus::BackingOff, Some(Reason::CrashLoop))
        );
        assert_eq!(d.restart_count, 3);
    }

    #[tokio::test]
    async fn the_backoff_elapsed_starts_an_instance() {
        let driver = FakeDriver::default();
        let mut state = RestartState {
            next_attempt_at: Some(now() + secs(60)),
            ..Default::default()
        };
        let mut d = worker(1);
        d.back_off(Reason::CrashLoop, "test");
        d.restart_count = 3;

        let d = pass(
            &driver,
            &RestartPolicy::default(),
            d,
            &mut state,
            now() + secs(61),
        )
        .await;

        assert_eq!(driver.starts(), 1);
        assert_eq!(d.status, DeploymentStatus::Progressing);
        assert_eq!(d.instances, ["new-1"], "the scale-up reports the new count");
        assert_eq!(state.next_attempt_at, None);
        assert_eq!(d.restart_count, 3, "a start is not a reset");

        // Seen still running on the next pass: now it is Running.
        let d = pass(
            &driver,
            &RestartPolicy::default(),
            d,
            &mut state,
            now() + secs(62),
        )
        .await;
        assert_eq!(d.instances, ["new-1"]);
        assert_eq!(d.status, DeploymentStatus::Running);
        assert_eq!(driver.starts(), 1);
    }

    #[tokio::test]
    async fn a_crash_looping_worker_is_never_abandoned() {
        let driver = FakeDriver::default();
        let policy = RestartPolicy::default();
        let mut state = RestartState::default();
        let mut d = worker(1);
        let mut t = now();
        for _ in 0..50 {
            // Wait out whatever backoff is pending, then let the new instance crash.
            t = state.next_attempt_at.unwrap_or(t).max(t) + secs(1);
            d = pass(&driver, &policy, d, &mut state, t).await;
            let id = driver.running.lock().unwrap().last().cloned();
            let id = id.expect("an instance was started");
            driver.crash(&id, Some(1));
            t += secs(15);
            d = pass(&driver, &policy, d, &mut state, t).await;
            assert_ne!(d.status, DeploymentStatus::Failed);
        }
        assert_eq!(d.restart_count, 50);
        assert_eq!(
            d.condition(),
            (DeploymentStatus::BackingOff, Some(Reason::CrashLoop))
        );
        let wait = state.next_attempt_at.map_or(0, |at| (at - t).num_seconds());
        assert!(wait <= 300, "never waits past the cap, waited {wait}s");
    }

    #[tokio::test]
    async fn on_exhaustion_fail_gives_up_after_max_attempts() {
        let driver = FakeDriver::default();
        let policy = RestartPolicy {
            on_exhaustion: OnExhaustion::Fail,
            max_attempts: 2,
            ..Default::default()
        };
        let mut state = RestartState::default();
        let mut d = worker(1);
        for (i, id) in ["a", "b", "c"].iter().enumerate() {
            driver.crash(id, Some(1));
            d = pass(
                &driver,
                &policy,
                d,
                &mut state,
                now() + secs(1000 * i as i64),
            )
            .await;
        }
        assert_eq!(d.status, DeploymentStatus::Failed);
        assert!(reasons(&d).contains(&"restart_limit_reached".to_string()));
    }

    #[tokio::test]
    async fn an_unrunnable_program_backs_off_as_spec_rejected() {
        let driver = FakeDriver::running(&["a"]);
        driver.crash("a", Some(127));
        let mut state = RestartState::default();

        let d = pass(
            &driver,
            &RestartPolicy::default(),
            worker(1),
            &mut state,
            now(),
        )
        .await;

        assert_eq!(
            d.condition(),
            (DeploymentStatus::BackingOff, Some(Reason::SpecRejected))
        );
        assert!(reasons(&d).contains(&"non_retryable_exit".to_string()));
    }

    #[tokio::test]
    async fn a_start_error_sets_its_status_and_backs_off() {
        let driver = FakeDriver::default();
        driver.fail_starts_with(RuntimeError::ConfigNotFound("app".to_string()));
        let mut state = RestartState::default();
        let mut d = worker(1);
        d.set_status(DeploymentStatus::Pending);

        let d = pass(&driver, &RestartPolicy::default(), d, &mut state, now()).await;

        assert_eq!(
            d.condition(),
            (DeploymentStatus::BackingOff, Some(Reason::ConfigMissing))
        );
        assert_eq!(d.restart_count, 1);
        assert!(reasons(&d).contains(&"config_error".to_string()));
    }

    /// One more replica fails to start while two serve: the deployment stays
    /// running, so their health checks keep running, and only the next start
    /// waits.
    #[tokio::test]
    async fn a_start_error_beside_serving_instances_keeps_the_deployment_running() {
        let driver = FakeDriver::running(&["a", "b"]);
        driver.fail_starts_with(RuntimeError::InsufficientResources("memory".to_string()));
        let mut state = RestartState::default();

        let d = pass(
            &driver,
            &RestartPolicy::default(),
            worker(3),
            &mut state,
            now(),
        )
        .await;

        assert_eq!(d.condition(), (DeploymentStatus::Running, None));
        assert_eq!(d.restart_count, 1);
        assert!(state.next_attempt_at.is_some());
        assert!(reasons(&d).contains(&"insufficient_resources".to_string()));
    }

    /// Backing off beside a serving instance, a deployment whose next start
    /// fails reports that failure, not the one it was backing off for.
    #[tokio::test]
    async fn a_start_error_while_backing_off_replaces_the_reason() {
        let driver = FakeDriver::running(&["a", "b"]);
        driver.crash("a", Some(127));
        let policy = RestartPolicy::default();
        let mut state = RestartState::default();

        let d = pass(&driver, &policy, worker(2), &mut state, now()).await;
        assert_eq!(
            d.condition(),
            (DeploymentStatus::BackingOff, Some(Reason::SpecRejected))
        );

        driver.fail_starts_with(RuntimeError::ConfigNotFound("app".to_string()));
        let later = now() + secs(301);
        let d = pass(&driver, &policy, d, &mut state, later).await;

        assert_eq!(
            d.condition(),
            (DeploymentStatus::BackingOff, Some(Reason::ConfigMissing))
        );
    }

    /// A rolling update's new deployment that cannot start all its replicas
    /// backs off, so the one it replaces is not drained meanwhile.
    #[tokio::test]
    async fn a_start_error_holds_a_rolling_update_back() {
        let driver = FakeDriver::running(&["a"]);
        driver.fail_starts_with(RuntimeError::InsufficientResources("memory".to_string()));
        let mut state = RestartState::default();
        let mut child = worker(2);
        child.parent_id = Some("previous".to_string());

        let d = pass(&driver, &RestartPolicy::default(), child, &mut state, now()).await;

        assert_eq!(
            d.condition(),
            (
                DeploymentStatus::BackingOff,
                Some(Reason::InsufficientResources)
            )
        );
    }

    #[tokio::test]
    async fn a_missing_config_is_picked_up_without_a_reapply() {
        let driver = FakeDriver::default();
        driver.fail_starts_with(RuntimeError::ConfigNotFound("app".to_string()));
        let policy = RestartPolicy::default();
        let mut state = RestartState::default();
        let mut d = worker(1);
        d.set_status(DeploymentStatus::Pending);

        d = pass(&driver, &policy, d, &mut state, now()).await;
        // The operator creates the config; the next attempt succeeds.
        let later = now() + secs(301);
        let d = pass(&driver, &policy, d, &mut state, later).await;

        assert_eq!(driver.starts(), 1);
        assert_eq!(d.status, DeploymentStatus::Progressing);
    }

    #[tokio::test]
    async fn the_counter_resets_after_stable_after() {
        let driver = FakeDriver::running(&["a"]);
        let policy = RestartPolicy::default();
        let mut state = RestartState::default();
        let mut d = worker(1);
        d.restart_count = 4;

        d = pass(&driver, &policy, d, &mut state, now()).await;
        assert_eq!(state.running_since, Some(now()));
        assert_eq!(d.restart_count, 4);

        d = pass(&driver, &policy, d, &mut state, now() + secs(9 * 60)).await;
        assert_eq!(d.restart_count, 4, "not stable for long enough yet");

        let d = pass(&driver, &policy, d, &mut state, now() + secs(10 * 60)).await;
        assert_eq!(d.restart_count, 0);
        assert!(reasons(&d).contains(&"restart_count_reset".to_string()));
    }

    #[tokio::test]
    async fn a_crash_restarts_the_stability_clock() {
        let driver = FakeDriver::running(&["a", "b"]);
        let policy = RestartPolicy::default();
        let mut state = RestartState::default();
        let mut d = worker(2);
        d.restart_count = 1;

        d = pass(&driver, &policy, d, &mut state, now()).await;
        driver.crash("a", Some(1));
        d = pass(&driver, &policy, d, &mut state, now() + secs(5 * 60)).await;
        assert_eq!(state.running_since, None);
        assert_eq!(d.status, DeploymentStatus::Running, "b is still serving");

        d = pass(&driver, &policy, d, &mut state, now() + secs(6 * 60)).await;
        let d = pass(&driver, &policy, d, &mut state, now() + secs(14 * 60)).await;
        assert_eq!(
            d.restart_count, 2,
            "10 minutes have not passed since the crash"
        );
    }

    #[tokio::test]
    async fn extra_instances_are_stopped_one_per_pass() {
        let driver = FakeDriver::running(&["a", "b", "c"]);
        let mut state = RestartState::default();

        let d = pass(
            &driver,
            &RestartPolicy::default(),
            worker(1),
            &mut state,
            now(),
        )
        .await;

        assert_eq!(*driver.stopped.lock().unwrap(), ["a"]);
        assert_eq!(d.instances, ["b", "c"]);
        assert_eq!(d.restart_count, 0, "a scale-down is not a failure");
        assert!(reasons(&d).contains(&"scale_down".to_string()));
    }

    #[tokio::test]
    async fn scaling_down_is_not_held_by_the_backoff() {
        let driver = FakeDriver::running(&["a", "b"]);
        let mut state = RestartState {
            next_attempt_at: Some(now() + secs(60)),
            ..Default::default()
        };

        pass(
            &driver,
            &RestartPolicy::default(),
            worker(1),
            &mut state,
            now(),
        )
        .await;

        assert_eq!(*driver.stopped.lock().unwrap(), ["a"]);
    }

    async fn job_pass(
        driver: &FakeDriver,
        backoff_limit: u32,
        deployment: Deployment,
        state: &mut RestartState,
        at: DateTime<Utc>,
    ) -> Deployment {
        let mut rng = StdRng::seed_from_u64(7);
        reconcile(
            driver,
            &RestartPolicy::default(),
            WorkloadKind::Job { backoff_limit },
            deployment,
            &[],
            state,
            at,
            &mut rng,
        )
        .await
    }

    fn job() -> Deployment {
        let mut d = worker(3);
        d.kind = "job".to_string();
        d.set_status(DeploymentStatus::Pending);
        d
    }

    #[tokio::test]
    async fn a_job_runs_one_instance_whatever_its_replicas() {
        let driver = FakeDriver::default();
        let mut state = RestartState::default();
        let mut d = job_pass(&driver, 0, job(), &mut state, now()).await;
        d = job_pass(&driver, 0, d, &mut state, now() + secs(1)).await;
        assert_eq!(driver.starts(), 1);
        assert_eq!(d.instances, ["new-1"]);
    }

    #[tokio::test]
    async fn an_instance_that_never_started_does_not_complete_a_job() {
        let driver = FakeDriver::default();
        driver.terminated.lock().unwrap().push(Termination {
            instance_id: "j".to_string(),
            exit_code: Some(0),
            finished_at: now(),
            started: false,
            logs_tail: None,
        });
        let mut state = RestartState::default();

        let d = job_pass(&driver, 0, job(), &mut state, now()).await;

        assert_ne!(d.status, DeploymentStatus::Completed);
        assert_ne!(d.status, DeploymentStatus::Failed);
        assert_eq!(state.run_failures, 0, "nothing ran");
        assert!(reasons(&d).contains(&"instance_never_started".to_string()));
    }

    #[tokio::test]
    async fn a_job_that_exits_zero_completes_and_retires_its_instance() {
        let driver = FakeDriver::running(&["j"]);
        driver.crash("j", Some(0));
        let mut state = RestartState::default();

        let d = job_pass(&driver, 0, job(), &mut state, now()).await;

        assert_eq!(d.status, DeploymentStatus::Completed);
        assert!(driver.discarded.lock().unwrap().is_empty());
        assert_eq!(*driver.retired.lock().unwrap(), ["j"]);
        assert!(reasons(&d).contains(&"job_completed".to_string()));
        assert_eq!(d.restart_count, 0);
    }

    #[tokio::test]
    async fn a_failed_job_is_not_run_again_by_default() {
        let driver = FakeDriver::running(&["j"]);
        driver.crash("j", Some(1));
        let mut state = RestartState::default();

        let d = job_pass(&driver, 0, job(), &mut state, now()).await;

        assert_eq!(d.status, DeploymentStatus::Failed);
        assert!(driver.discarded.lock().unwrap().is_empty());
        assert_eq!(*driver.retired.lock().unwrap(), ["j"]);
        assert_eq!(driver.starts(), 0);
        assert_eq!(state.run_failures, 1);
    }

    #[tokio::test]
    async fn a_failed_job_is_run_again_up_to_its_backoff_limit() {
        let driver = FakeDriver::default();
        let mut state = RestartState::default();
        let mut d = job();
        let mut t = now();
        for run in 1..=3 {
            t = state.next_attempt_at.unwrap_or(t).max(t) + secs(1);
            d = job_pass(&driver, 2, d, &mut state, t).await;
            let id = driver.running.lock().unwrap().last().cloned().unwrap();
            driver.crash(&id, Some(1));
            t += secs(1);
            d = job_pass(&driver, 2, d, &mut state, t).await;
            assert_eq!(state.run_failures, run);
        }
        assert_eq!(driver.starts(), 3);
        assert_eq!(d.status, DeploymentStatus::Failed);
    }

    #[tokio::test]
    async fn a_job_that_cannot_start_keeps_retrying() {
        let driver = FakeDriver::default();
        let mut state = RestartState::default();
        let mut d = job();
        let mut t = now();
        for _ in 0..10 {
            driver.fail_starts_with(RuntimeError::ImagePullFailed("registry down".to_string()));
            t = state.next_attempt_at.unwrap_or(t).max(t) + secs(1);
            d = job_pass(&driver, 0, d, &mut state, t).await;
            assert_ne!(d.status, DeploymentStatus::Failed);
        }
        assert_eq!(d.restart_count, 10);
        assert_eq!(state.run_failures, 0);

        // The registry is back: the job finally runs.
        t = state.next_attempt_at.unwrap_or(t).max(t) + secs(1);
        job_pass(&driver, 0, d, &mut state, t).await;
        assert_eq!(driver.starts(), 1);
    }

    #[tokio::test]
    async fn a_worker_whose_last_instance_was_killed_is_not_running() {
        let policy = RestartPolicy::default();
        let driver = FakeDriver::default();
        let mut state = RestartState::default();
        let mut d = worker(1);
        let mut rng = StdRng::seed_from_u64(1);

        // The liveness check removed the only instance.
        record_removals(
            &policy,
            WorkloadKind::Worker,
            &mut d,
            &mut state,
            1,
            Reason::CrashLoop,
            "liveness",
            now(),
            &mut rng,
        );
        assert_eq!(
            d.condition(),
            (DeploymentStatus::BackingOff, Some(Reason::CrashLoop))
        );

        // Its replacement starts progressing, so it goes through the readiness
        // gate like any other start.
        let at = state.next_attempt_at.unwrap_or(now()) + secs(1);
        let d = pass(&driver, &policy, d, &mut state, at).await;
        assert_eq!(d.status, DeploymentStatus::Progressing);
    }

    #[tokio::test]
    async fn an_instance_that_dies_on_start_is_never_reported_running() {
        let driver = FakeDriver::default();
        let policy = RestartPolicy::default();
        let mut state = RestartState::default();
        let mut d = worker(1);
        d.set_status(DeploymentStatus::Pending);

        d = pass(&driver, &policy, d, &mut state, now()).await;
        assert_eq!(d.status, DeploymentStatus::Progressing);
        driver.crash("new-1", Some(1));
        let d = pass(&driver, &policy, d, &mut state, now() + secs(1)).await;
        assert_eq!(
            d.condition(),
            (DeploymentStatus::BackingOff, Some(Reason::CrashLoop))
        );
    }

    #[test]
    fn a_job_killed_by_its_health_check_is_a_failed_run() {
        let mut state = RestartState::default();
        let mut d = job();
        d.set_status(DeploymentStatus::Running);
        let mut rng = StdRng::seed_from_u64(1);

        record_removals(
            &RestartPolicy::default(),
            WorkloadKind::Job { backoff_limit: 0 },
            &mut d,
            &mut state,
            1,
            Reason::RunFailed,
            "liveness",
            now(),
            &mut rng,
        );

        assert_eq!(d.status, DeploymentStatus::Failed);
        assert_eq!(state.run_failures, 1);
    }

    /// A readiness timeout backs off like a crash, with its own reason, and
    /// waits on the backoff curve before the instances come back. However
    /// long the instances were up, the budget is not given back: they were
    /// never ready.
    #[test]
    fn a_readiness_timeout_backs_off_for_its_reason() {
        let policy = RestartPolicy::default();
        let mut state = RestartState {
            running_since: Some(now()),
            ..Default::default()
        };
        let mut d = worker(1);
        d.instances.clear();
        d.restart_count = 3;
        let mut rng = StdRng::seed_from_u64(1);
        let deadline = now() + policy.stable_after + secs(60);

        record_readiness_timeout(
            &policy,
            &mut d,
            &mut state,
            "never ready",
            deadline,
            &mut rng,
        );

        assert_eq!(
            d.condition(),
            (DeploymentStatus::BackingOff, Some(Reason::ReadinessTimeout))
        );
        assert_eq!(d.message.as_deref(), Some("never ready"));
        assert_eq!(d.restart_count, 4, "counted on top of the earlier failures");
        assert!(state.next_attempt_at.is_some());
    }

    /// A failed job says why: its run failed, with the exit code.
    #[tokio::test]
    async fn a_failed_job_carries_its_reason_and_message() {
        let driver = FakeDriver::running(&["j"]);
        driver.crash("j", Some(3));
        let mut state = RestartState::default();

        let d = job_pass(&driver, 0, job(), &mut state, now()).await;

        assert_eq!(
            d.condition(),
            (DeploymentStatus::Failed, Some(Reason::RunFailed))
        );
        assert_eq!(d.message.as_deref(), Some("Instance j exited with code 3"));
    }

    /// A worker held back by the readiness gate does not earn its budget back
    /// for staying up: it is not serving.
    #[tokio::test]
    async fn the_counter_resets_only_once_ready() {
        let driver = FakeDriver::running(&["a"]);
        let policy = RestartPolicy::default();
        let mut state = RestartState {
            running_since: Some(now()),
            ..Default::default()
        };
        let mut held = worker(1);
        held.restart_count = 3;
        held.set_status(DeploymentStatus::Progressing);
        let later = now() + policy.stable_after + secs(1);

        let d = pass(&driver, &policy, held, &mut state, later).await;
        assert_eq!(d.restart_count, 3, "still progressing: not forgiven");

        let mut ready = worker(1);
        ready.restart_count = 3;
        let d = pass(&driver, &policy, ready, &mut state, later).await;
        assert_eq!(d.restart_count, 0, "running: forgiven");
    }

    /// An instance that stays up without ever being ready, then exits, is one
    /// more failure: the length of a run that never served is not forgiven.
    #[tokio::test]
    async fn a_crash_after_a_long_unready_run_is_not_forgiven() {
        let driver = FakeDriver::running(&["a"]);
        driver.crash("a", Some(1));
        let policy = RestartPolicy::default();
        let mut state = RestartState {
            running_since: Some(now()),
            ..Default::default()
        };
        let mut held = worker(1);
        held.restart_count = 3;
        held.set_status(DeploymentStatus::Progressing);
        let later = now() + policy.stable_after + secs(1);

        let d = pass(&driver, &policy, held, &mut state, later).await;

        assert_eq!(d.restart_count, 4);
    }

    /// Backing off for a program that cannot run while another instance
    /// serves, then scaled down to that instance: nothing is left to start, so
    /// the deployment goes back through progressing instead of staying stuck.
    #[tokio::test]
    async fn a_scale_down_to_the_serving_instances_ends_the_back_off() {
        let driver = FakeDriver::running(&["a", "b"]);
        driver.crash("a", Some(127));
        let policy = RestartPolicy::default();
        let mut state = RestartState::default();

        let d = pass(&driver, &policy, worker(2), &mut state, now()).await;
        assert_eq!(
            d.condition(),
            (DeploymentStatus::BackingOff, Some(Reason::SpecRejected))
        );

        let mut scaled = d;
        scaled.replicas = 1;
        let d = pass(&driver, &policy, scaled, &mut state, now()).await;
        assert_eq!(d.condition(), (DeploymentStatus::Progressing, None));
        assert_eq!(driver.starts(), 0);

        let d = pass(&driver, &policy, d, &mut state, now()).await;
        assert_eq!(d.status, DeploymentStatus::Running);
    }

    /// Two replicas end in the same pass, one unable to run its program: the
    /// reason says so, whatever the order they are reported in.
    #[tokio::test]
    async fn a_program_that_cannot_run_wins_over_a_crash_in_the_same_pass() {
        let driver = FakeDriver::running(&["a", "b"]);
        driver.crash("a", Some(127));
        driver.crash("b", Some(1));
        let mut state = RestartState::default();

        let d = pass(
            &driver,
            &RestartPolicy::default(),
            worker(2),
            &mut state,
            now(),
        )
        .await;

        assert_eq!(
            d.condition(),
            (DeploymentStatus::BackingOff, Some(Reason::SpecRejected))
        );
    }

    #[test]
    fn liveness_kills_count_as_failures() {
        let policy = RestartPolicy::default();
        let mut state = RestartState {
            running_since: Some(now()),
            ..Default::default()
        };
        let mut d = worker(1);
        let mut rng = StdRng::seed_from_u64(1);

        record_removals(
            &policy,
            WorkloadKind::Worker,
            &mut d,
            &mut state,
            2,
            Reason::CrashLoop,
            "liveness",
            now(),
            &mut rng,
        );

        assert_eq!(d.restart_count, 2);
        assert_eq!(state.running_since, None);
    }

    #[test]
    fn stable_after_reads_naturally() {
        assert_eq!(humanize(Duration::from_secs(600)), "10m");
        assert_eq!(humanize(Duration::from_secs(45)), "45s");
    }
}
