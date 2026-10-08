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
//!   is kept, with its logs. A job that cannot start is retried without limit.

use crate::hypervisor::classifier::{
    Disposition, classify_create_error, classify_exit_code, create_error_outcome,
};
use crate::hypervisor::instance_driver::InstanceDriver;
use crate::models::deployments::{Deployment, DeploymentStatus};
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

    let observation = driver.observe(&deployment).await;
    deployment.instances = observation.running;

    let failed_this_pass = !observation.terminated.is_empty();
    for termination in observation.terminated {
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
            deployment.status = DeploymentStatus::Completed;
            deployment.emit_event(
                "info",
                "Job completed".to_string(),
                "scheduler",
                Some("job_completed"),
            );
            state.last_termination = Some(termination);
            state.next_attempt_at = None;
            break;
        }
        deployment.emit_event(
            "error",
            format!(
                "Instance {} exited with code {} (restart {})",
                short_id(&termination.instance_id),
                termination
                    .exit_code
                    .map_or_else(|| "unknown".to_string(), |c| c.to_string()),
                counter.restart_count
            ),
            &deployment.runtime.clone(),
            Some("container_crashed"),
        );
        if let Disposition::Terminal(status) = classify_exit_code(termination.exit_code) {
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
            deployment.status = status;
        }
        // A finished job keeps its instance, and the logs with it.
        let job_done = matches!(kind, WorkloadKind::Job { .. }) && decision == Decision::Fail;
        if !job_done {
            driver.discard_instance(&termination.instance_id).await;
        }
        state.last_termination = Some(termination);
        apply_decision(&mut deployment, state, decision, counter.restart_count);
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
    if failed_this_pass
        && deployment.instances.is_empty()
        && deployment.status != DeploymentStatus::CreateContainerError
    {
        deployment.status = DeploymentStatus::CrashLoopBackOff;
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

    if deployment.instances.is_empty() {
        counter.running_since = None;
    } else if !failed_this_pass && kind == WorkloadKind::Worker {
        restart::on_running(&mut counter, now);
        if restart::reset_if_stable(policy, &mut counter, now) {
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

/// Count instances a liveness check removed as failures, exactly like
/// instances that exited on their own.
pub(crate) fn record_liveness_kills(
    policy: &RestartPolicy,
    deployment: &mut Deployment,
    state: &mut RestartState,
    kills: usize,
    now: DateTime<Utc>,
    rng: &mut impl Rng,
) {
    let mut counter = restart::RestartState {
        restart_count: deployment.restart_count,
        running_since: state.running_since,
        run_failures: state.run_failures,
    };
    for _ in 0..kills {
        let decision = restart::decide(
            policy,
            WorkloadKind::Worker,
            &mut counter,
            Event::Exited { exit_code: None },
            now,
            rng,
        );
        apply_decision(deployment, state, decision, counter.restart_count);
    }
    deployment.restart_count = counter.restart_count;
    state.running_since = counter.running_since;
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
            // The new instance only counts once the next pass has seen it
            // running. Counted now, a container that dies on start would be
            // promoted to Running for a pass, and a worker restarted from a
            // failure status would reach Running without going through the
            // readiness gate, which only holds a deployment that was Creating.
            deployment.instances.pop();
            if deployment.status != DeploymentStatus::Running {
                deployment.status = DeploymentStatus::Creating;
            }
        }
        Err(err) => {
            let (status, reason, message) = create_error_outcome(&err, deployment);
            error!("[{}] {}: {}", deployment.id, reason, err);
            deployment.emit_event("error", message, &deployment.runtime.clone(), Some(reason));
            deployment.status = status;

            let failure = match classify_create_error(&err) {
                Disposition::Terminal(
                    DeploymentStatus::ConfigError
                    | DeploymentStatus::CreateContainerError
                    | DeploymentStatus::Failed,
                ) => StartFailure::NeedsOperator,
                _ => StartFailure::Transient,
            };
            let decision =
                restart::decide(policy, kind, counter, Event::StartFailed(failure), now, rng);
            apply_decision(deployment, state, decision, counter.restart_count);
        }
    }
}

fn apply_decision(
    deployment: &mut Deployment,
    state: &mut RestartState,
    decision: Decision,
    attempts: u32,
) {
    match decision {
        Decision::StartNow | Decision::Complete => state.next_attempt_at = None,
        Decision::WaitUntil(at) => state.next_attempt_at = Some(at),
        Decision::Fail => {
            state.next_attempt_at = None;
            if deployment.status != DeploymentStatus::Failed {
                deployment.status = DeploymentStatus::Failed;
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
        async fn observe(&self, _deployment: &Deployment) -> Observation {
            Observation {
                running: self.running.lock().unwrap().clone(),
                terminated: self.terminated.lock().unwrap().clone(),
            }
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
        d.status = DeploymentStatus::CrashLoopBackOff;
        d.restart_count = 3;

        let d = pass(&driver, &RestartPolicy::default(), d, &mut state, now()).await;

        assert_eq!(driver.starts(), 0);
        assert_eq!(d.status, DeploymentStatus::CrashLoopBackOff);
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
        d.status = DeploymentStatus::CrashLoopBackOff;
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
        assert_eq!(d.status, DeploymentStatus::Creating);
        assert!(
            d.instances.is_empty(),
            "a new instance counts once it has been seen running"
        );
        assert_eq!(state.next_attempt_at, None);
        assert_eq!(d.restart_count, 3, "a start is not a reset");

        // Seen running on the next pass: now it counts.
        let d = pass(
            &driver,
            &RestartPolicy::default(),
            d,
            &mut state,
            now() + secs(62),
        )
        .await;
        assert_eq!(d.instances, ["new-1"]);
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
        assert_eq!(d.status, DeploymentStatus::CrashLoopBackOff);
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
    async fn an_unrunnable_program_lands_on_create_container_error() {
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

        assert_eq!(d.status, DeploymentStatus::CreateContainerError);
        assert!(reasons(&d).contains(&"non_retryable_exit".to_string()));
    }

    #[tokio::test]
    async fn a_start_error_sets_its_status_and_backs_off() {
        let driver = FakeDriver::default();
        driver.fail_starts_with(RuntimeError::ConfigNotFound("app".to_string()));
        let mut state = RestartState::default();
        let mut d = worker(1);
        d.status = DeploymentStatus::Pending;

        let d = pass(&driver, &RestartPolicy::default(), d, &mut state, now()).await;

        assert_eq!(d.status, DeploymentStatus::ConfigError);
        assert_eq!(d.restart_count, 1);
        assert!(reasons(&d).contains(&"config_error".to_string()));
    }

    #[tokio::test]
    async fn a_missing_config_is_picked_up_without_a_reapply() {
        let driver = FakeDriver::default();
        driver.fail_starts_with(RuntimeError::ConfigNotFound("app".to_string()));
        let policy = RestartPolicy::default();
        let mut state = RestartState::default();
        let mut d = worker(1);
        d.status = DeploymentStatus::Pending;

        d = pass(&driver, &policy, d, &mut state, now()).await;
        // The operator creates the config; the next attempt succeeds.
        let later = now() + secs(301);
        let d = pass(&driver, &policy, d, &mut state, later).await;

        assert_eq!(driver.starts(), 1);
        assert_eq!(d.status, DeploymentStatus::Creating);
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
        d.status = DeploymentStatus::Pending;
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
    async fn a_job_that_exits_zero_completes_and_keeps_its_instance() {
        let driver = FakeDriver::running(&["j"]);
        driver.crash("j", Some(0));
        let mut state = RestartState::default();

        let d = job_pass(&driver, 0, job(), &mut state, now()).await;

        assert_eq!(d.status, DeploymentStatus::Completed);
        assert!(driver.discarded.lock().unwrap().is_empty());
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
        assert!(driver.discarded.lock().unwrap().is_empty(), "logs are kept");
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

    #[test]
    fn liveness_kills_count_as_failures() {
        let policy = RestartPolicy::default();
        let mut state = RestartState {
            running_since: Some(now()),
            ..Default::default()
        };
        let mut d = worker(1);
        let mut rng = StdRng::seed_from_u64(1);

        record_liveness_kills(&policy, &mut d, &mut state, 2, now(), &mut rng);

        assert_eq!(d.restart_count, 2);
        assert_eq!(state.running_since, None);
    }

    #[test]
    fn stable_after_reads_naturally() {
        assert_eq!(humanize(Duration::from_secs(600)), "10m");
        assert_eq!(humanize(Duration::from_secs(45)), "45s");
    }
}
