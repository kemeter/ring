//! Shared retry-vs-give-up classifier for runtime failures.
//!
//! Two boundaries decide whether a failed deployment should be retried or fail
//! fast:
//!
//! * the **create boundary** — a runtime rejected `create`/`start`/boot with a
//!   [`RuntimeError`];
//! * the **crash boundary** — a worker container started, then exited with some
//!   exit code.
//!
//! Both used to funnel every failure into the same "bump `restart_count`, retry
//! up to `MAX_RESTART_COUNT`" loop, so a permanent problem (image that doesn't
//! exist, missing config, a binary that isn't executable) still burned five
//! reconcile cycles before surfacing. This classifier folds the
//! permanent-vs-transient decision into one place so a non-retryable failure
//! lands on its terminal status immediately.
//!
//! It generalises the Cloud Hypervisor runtime's `classify_vm_start_error`. It is
//! deliberately runtime-agnostic (it only reads the shared [`RuntimeError`] enum
//! and a raw exit code): Docker and Podman go through [`classify_create_error`]
//! and [`classify_exit_code`] directly, while the VM runtimes (Cloud Hypervisor,
//! Firecracker) share [`classify_vm_start_error`], which layers an event reason
//! on top of the same verdict.

use crate::hypervisor::error::RuntimeError;
use crate::models::deployments::DeploymentStatus;

/// What to do with a failed deployment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Disposition {
    /// Permanent failure: set this status and stop retrying. Retrying would only
    /// burn reconcile cycles without changing the outcome.
    Terminal(DeploymentStatus),
    /// Transient failure: count it toward `restart_count` and retry, converging
    /// to a terminal `CrashLoopBackOff`/`Failed` only if it keeps failing.
    Retry,
}

impl Disposition {
    pub(crate) fn is_terminal(&self) -> bool {
        matches!(self, Disposition::Terminal(_))
    }
}

/// Classify a create-boundary [`RuntimeError`] into terminal-or-retry while
/// preserving the status each error maps to. The status mapping mirrors the
/// Docker runtime's `handle_create_error`; this only adds the terminal-vs-retry
/// verdict on top.
///
/// Terminal: the failure can't fix itself on a retry — the image truly doesn't
/// exist, the config/key is absent, the container spec is rejected, firmware is
/// missing, or the host is out of memory. Retry: registry/network hiccups, a
/// busy port, a VM that failed to boot, and other transient/unknown errors.
pub(crate) fn classify_create_error(err: &RuntimeError) -> Disposition {
    match err {
        // Permanent: the image isn't there (or policy forbids pulling it).
        RuntimeError::ImageNotFound(_) => Disposition::Terminal(DeploymentStatus::ImagePullBackOff),
        // Transient: a pull that failed mid-flight (registry/network) can succeed
        // on a retry.
        RuntimeError::ImagePullFailed(_) => Disposition::Retry,
        // Permanent: Docker rejecting `create`/`start` almost always means a bad
        // container spec (entrypoint, mount, options) — retrying re-submits the
        // same rejected spec. Fail fast onto CreateContainerError.
        //
        // Caveat for adopters: this verdict assumes the variant carries a
        // *rejected spec*, as it does on Docker. The containerd runtime reuses it
        // for any failed gRPC call in the create path (including a transient shim
        // outage), so it deliberately overrides this one case back to Retry — see
        // `containerd::lifecycle::handle_create_error`.
        RuntimeError::InstanceCreationFailed(_) => {
            Disposition::Terminal(DeploymentStatus::CreateContainerError)
        }
        // Transient: network setup can race with daemon/host state.
        RuntimeError::NetworkCreationFailed(_) => Disposition::Retry,
        // Permanent: the referenced config (or key) is absent — a retry won't
        // conjure it. The operator must create it.
        RuntimeError::ConfigNotFound(_) | RuntimeError::ConfigKeyNotFound(_) => {
            Disposition::Terminal(DeploymentStatus::ConfigError)
        }
        // Permanent: firmware/kernel file missing at the configured path.
        RuntimeError::FirmwareNotFound(_) => Disposition::Terminal(DeploymentStatus::Failed),
        // Permanent: the host is short on memory now; a retry won't free any.
        RuntimeError::InsufficientResources(_) => {
            Disposition::Terminal(DeploymentStatus::InsufficientResources)
        }
        // Transient: another process holds the port; it may be released.
        RuntimeError::PortAlreadyInUse(_) => Disposition::Retry,
        // Transient/unknown: worth a retry within the restart budget.
        RuntimeError::VmStartFailed(_)
        | RuntimeError::StatsFetchFailed(_)
        | RuntimeError::Other(_)
        | RuntimeError::Io(_)
        | RuntimeError::Json(_) => Disposition::Retry,
    }
}

/// Classify a crash-boundary exit code into terminal-or-retry.
///
/// A worker that exits is normally restarted (transient): the process may have
/// hit a one-off error and a fresh start can recover. Two exit codes are the
/// exception because they are *unambiguously* permanent under the standard shell
/// convention, so a restart can never succeed:
///
/// * `0` — the process ran to completion successfully. A worker that exits 0
///   has *finished*, not crashed: it must converge to `Completed`, never be
///   recreated. Treating it as retryable recreates the container every tick
///   forever (re-pulling the image each time under the default `Always`
///   policy) — a one-shot/`pg_dump`-style container declared as a worker would
///   otherwise loop endlessly and starve the reconcile cycle.
/// * `127` — command not found (the entrypoint/binary doesn't exist);
/// * `126` — found but not executable (bad perms / not a binary).
///
/// 126/127 mean the container can never start its program, so we fail fast onto
/// `CreateContainerError` rather than burning the whole restart budget. Every
/// other code (generic `1`, signal-kill `128+n`) stays retryable — those can be
/// transient, and mislabelling them terminal would wrongly give up on a
/// recoverable worker.
pub(crate) fn classify_exit_code(exit_code: Option<i64>) -> Disposition {
    match exit_code {
        Some(0) => Disposition::Terminal(DeploymentStatus::Completed),
        Some(126) | Some(127) => Disposition::Terminal(DeploymentStatus::CreateContainerError),
        _ => Disposition::Retry,
    }
}

/// Classify a VM start failure for the microVM runtimes (Cloud Hypervisor,
/// Firecracker), returning the terminal status to land on — or `None` when the
/// failure is transient and should bump `restart_count` — alongside the event
/// reason to record.
///
/// The verdict comes from [`classify_create_error`], so the VM runtimes converge
/// exactly like Docker: a missing config, a rejected instance spec or absent
/// firmware fails fast instead of burning the whole restart budget. Only the
/// event reason is runtime-flavoured, which is why this wrapper exists at all.
pub(crate) fn classify_vm_start_error(
    e: &RuntimeError,
) -> (Option<DeploymentStatus>, &'static str) {
    let reason = match e {
        RuntimeError::FirmwareNotFound(_) => "FirmwareNotFound",
        RuntimeError::ImageNotFound(_) => "ImageNotFound",
        RuntimeError::ImagePullFailed(_) => "ImagePullFailed",
        RuntimeError::InstanceCreationFailed(_) => "InstanceCreationFailed",
        RuntimeError::ConfigNotFound(_) | RuntimeError::ConfigKeyNotFound(_) => "ConfigError",
        RuntimeError::InsufficientResources(_) => "insufficient_resources",
        RuntimeError::PortAlreadyInUse(_) => "PortAllocationFailed",
        RuntimeError::NetworkCreationFailed(_) => "NetworkCreationFailed",
        _ => "VmStartFailed",
    };

    let status = match classify_create_error(e) {
        Disposition::Terminal(status) => Some(status),
        Disposition::Retry => None,
    };

    (status, reason)
}

/// Apply a VM start failure to `deployment`: record the event, then either land
/// on the terminal status or consume one unit of the restart budget.
///
/// `bound_status` is where a *transient* failure converges once the budget runs
/// out — `CrashLoopBackOff` for a worker, `Failed` for a job.
///
/// Both branches must move `restart_count`. Several terminal statuses
/// (`image_pull_back_off`, `config_error`, `create_container_error`) are still
/// polled by the reconcile loop, so setting the status without pushing the
/// counter to the bound would retry a permanent failure on every tick forever —
/// exactly the loop this classifier exists to stop.
/// True when the scheduler already refuses to reconcile a deployment in this
/// status, so no restart-budget marker is needed to stop the retries.
///
/// Derived from [`RECONCILED_STATUSES`] rather than listed by hand: the two
/// must stay exact complements. A status ADDED to the reconcile filter while
/// still reported as skipped here would be retried forever, with no budget to
/// stop it — which is why this reads the same array the scheduler queries with
/// instead of duplicating it.
pub(crate) fn scheduler_skips_by_status(status: &DeploymentStatus) -> bool {
    !crate::models::deployments::RECONCILED_STATUSES.contains(status)
}

pub(crate) fn apply_vm_start_failure(
    deployment: &mut crate::models::deployments::Deployment,
    err: &RuntimeError,
    runtime: &str,
    bound_status: DeploymentStatus,
) {
    use crate::models::deployments::MAX_RESTART_COUNT;

    let (status, reason) = classify_vm_start_error(err);
    deployment.emit_event("error", format!("{}", err), runtime, Some(reason));

    match status {
        Some(terminal) => {
            // Exhausting the restart budget is how a terminal error stops the
            // scheduler from retrying — but only for statuses the scheduler
            // still reconciles (`ConfigError`, `ImagePullBackOff`,
            // `CreateContainerError`, ...). A few terminal statuses are already
            // excluded from its filter by status alone; for those the marker
            // buys nothing and makes the field lie, so it is skipped.
            //
            // Concretely: a deployment refused by host-memory admission used to
            // report 5 restarts having never started a single process, sending
            // whoever read it looking for an instability that never existed.
            if !scheduler_skips_by_status(&terminal) {
                deployment.restart_count = MAX_RESTART_COUNT;
            }
            deployment.status = terminal;
        }
        None => {
            deployment.restart_count += 1;
            if deployment.restart_count >= MAX_RESTART_COUNT {
                deployment.status = bound_status;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_not_found_is_terminal_image_pull_back_off() {
        let d = classify_create_error(&RuntimeError::ImageNotFound("x".into()));
        assert_eq!(d, Disposition::Terminal(DeploymentStatus::ImagePullBackOff));
        assert!(d.is_terminal());
    }

    #[test]
    fn image_pull_failed_is_retry() {
        assert_eq!(
            classify_create_error(&RuntimeError::ImagePullFailed("net".into())),
            Disposition::Retry
        );
    }

    #[test]
    fn instance_creation_failed_is_terminal_create_container_error() {
        assert_eq!(
            classify_create_error(&RuntimeError::InstanceCreationFailed("bad".into())),
            Disposition::Terminal(DeploymentStatus::CreateContainerError)
        );
    }

    #[test]
    fn config_errors_are_terminal() {
        assert_eq!(
            classify_create_error(&RuntimeError::ConfigNotFound("c".into())),
            Disposition::Terminal(DeploymentStatus::ConfigError)
        );
        assert_eq!(
            classify_create_error(&RuntimeError::ConfigKeyNotFound("k".into())),
            Disposition::Terminal(DeploymentStatus::ConfigError)
        );
    }

    #[test]
    fn transient_runtime_errors_retry() {
        assert_eq!(
            classify_create_error(&RuntimeError::NetworkCreationFailed("n".into())),
            Disposition::Retry
        );
        assert_eq!(
            classify_create_error(&RuntimeError::PortAlreadyInUse(8080)),
            Disposition::Retry
        );
        assert_eq!(
            classify_create_error(&RuntimeError::Other("boom".into())),
            Disposition::Retry
        );
    }

    /// A deployment refused before anything ran must not claim restarts it
    /// never made. `restart_count` is displayed by the CLI, the API and the
    /// dashboard as a count of actual restarts; reporting 5 for a workload that
    /// never started a process sends an operator hunting a phantom instability.
    #[test]
    fn admission_refusal_does_not_invent_restarts() {
        let mut d = vm_deployment();
        assert_eq!(d.restart_count, 0);

        apply_vm_start_failure(
            &mut d,
            &RuntimeError::InsufficientResources("needs 4096 MiB but only 1800 MiB".into()),
            "firecracker",
            DeploymentStatus::CrashLoopBackOff,
        );

        assert_eq!(d.status, DeploymentStatus::InsufficientResources);
        assert_eq!(
            d.restart_count, 0,
            "no VM was ever spawned, so no restart may be reported"
        );
    }

    /// The counterpart: statuses the scheduler DOES keep reconciling still need
    /// the exhausted budget, otherwise they would be retried forever.
    #[test]
    fn a_reconciled_terminal_status_still_exhausts_the_budget() {
        let mut d = vm_deployment();

        apply_vm_start_failure(
            &mut d,
            &RuntimeError::ConfigNotFound("missing-config".into()),
            "firecracker",
            DeploymentStatus::CrashLoopBackOff,
        );

        assert_eq!(d.status, DeploymentStatus::ConfigError);
        assert_eq!(
            d.restart_count,
            crate::models::deployments::MAX_RESTART_COUNT,
            "ConfigError is still reconciled, so the budget is what stops the retries"
        );
    }

    /// The two sides must be exact complements. This no longer duplicates the
    /// scheduler's list — both derive from `RECONCILED_STATUSES` — so the test
    /// checks the property rather than a copy that could go stale.
    #[test]
    fn skipped_and_reconciled_statuses_are_complements() {
        use crate::models::deployments::RECONCILED_STATUSES;

        for status in DeploymentStatus::all() {
            assert_eq!(
                scheduler_skips_by_status(&status),
                !RECONCILED_STATUSES.contains(&status),
                "{status:?} is inconsistent between the reconcile filter and the skip check"
            );
        }

        // Sanity: neither side is empty, which would make the assertion above
        // vacuously true.
        assert!(!RECONCILED_STATUSES.is_empty());
        assert!(
            DeploymentStatus::all()
                .iter()
                .any(scheduler_skips_by_status),
            "no status is skipped — the marker would always be written"
        );
    }

    #[test]
    fn insufficient_resources_is_terminal() {
        assert_eq!(
            classify_create_error(&RuntimeError::InsufficientResources("need".into())),
            Disposition::Terminal(DeploymentStatus::InsufficientResources)
        );
    }

    #[test]
    fn unexecutable_and_missing_command_exit_codes_are_terminal() {
        assert_eq!(
            classify_exit_code(Some(126)),
            Disposition::Terminal(DeploymentStatus::CreateContainerError)
        );
        assert_eq!(
            classify_exit_code(Some(127)),
            Disposition::Terminal(DeploymentStatus::CreateContainerError)
        );
    }

    #[test]
    fn clean_exit_completes() {
        // A successful exit (code 0) is terminal-Completed, never retried — a
        // worker that finished must not be recreated in a loop.
        assert_eq!(
            classify_exit_code(Some(0)),
            Disposition::Terminal(DeploymentStatus::Completed)
        );
    }

    #[test]
    fn vm_start_missing_firmware_is_terminal_failed() {
        // Firecracker used to fall through to the catch-all here and retry a
        // missing kernel/rootfs five times over.
        assert_eq!(
            classify_vm_start_error(&RuntimeError::FirmwareNotFound("/no/kernel".into())),
            (Some(DeploymentStatus::Failed), "FirmwareNotFound")
        );
    }

    #[test]
    fn vm_start_config_errors_are_terminal() {
        assert_eq!(
            classify_vm_start_error(&RuntimeError::ConfigNotFound("c".into())),
            (Some(DeploymentStatus::ConfigError), "ConfigError")
        );
        assert_eq!(
            classify_vm_start_error(&RuntimeError::ConfigKeyNotFound("k".into())),
            (Some(DeploymentStatus::ConfigError), "ConfigError")
        );
    }

    #[test]
    fn vm_start_instance_creation_failed_is_terminal() {
        assert_eq!(
            classify_vm_start_error(&RuntimeError::InstanceCreationFailed("bad spec".into())),
            (
                Some(DeploymentStatus::CreateContainerError),
                "InstanceCreationFailed"
            )
        );
    }

    #[test]
    fn vm_start_transient_errors_have_no_terminal_status() {
        assert_eq!(
            classify_vm_start_error(&RuntimeError::PortAlreadyInUse(8080)),
            (None, "PortAllocationFailed")
        );
        assert_eq!(
            classify_vm_start_error(&RuntimeError::VmStartFailed("boot".into())),
            (None, "VmStartFailed")
        );
        assert_eq!(
            classify_vm_start_error(&RuntimeError::ImagePullFailed("net".into())),
            (None, "ImagePullFailed")
        );
    }

    #[test]
    fn vm_start_matches_create_error_verdict() {
        // The two entry points must never drift: same error, same terminal-ness.
        let errors = [
            RuntimeError::FirmwareNotFound("f".into()),
            RuntimeError::ImageNotFound("i".into()),
            RuntimeError::ImagePullFailed("p".into()),
            RuntimeError::InstanceCreationFailed("c".into()),
            RuntimeError::ConfigNotFound("c".into()),
            RuntimeError::InsufficientResources("m".into()),
            RuntimeError::PortAlreadyInUse(1),
            RuntimeError::NetworkCreationFailed("n".into()),
            RuntimeError::Other("x".into()),
        ];
        for err in &errors {
            let (status, _) = classify_vm_start_error(err);
            assert_eq!(
                status.is_some(),
                classify_create_error(err).is_terminal(),
                "verdict drifted for {:?}",
                err
            );
        }
    }

    fn vm_deployment() -> crate::models::deployments::Deployment {
        crate::models::deployments::Deployment {
            id: "vm1".to_string(),
            created_at: chrono::Utc::now().to_string(),
            updated_at: None,
            status: DeploymentStatus::Creating,
            restart_count: 0,
            namespace: "test".to_string(),
            name: "vm".to_string(),
            image: "/var/lib/ring/rootfs.ext4".to_string(),
            config: None,
            runtime: "firecracker".to_string(),
            kind: "worker".to_string(),
            replicas: 1,
            command: vec![],
            instances: vec![],
            labels: std::collections::HashMap::new(),
            environment: std::collections::HashMap::new(),
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
        }
    }

    /// The invariant that stops the infinite reboot loop: a failure must either
    /// move `restart_count` or land on a status the scheduler refuses to
    /// reconcile. Leaving BOTH untouched would retry a permanent failure on
    /// every tick, forever.
    ///
    /// The counter is not required on its own: several terminal statuses are
    /// already excluded from the reconcile filter, and marking those would only
    /// report restarts that never happened.
    #[test]
    fn every_start_failure_either_counts_or_lands_outside_the_reconcile_filter() {
        let errors = [
            RuntimeError::FirmwareNotFound("f".into()),
            RuntimeError::ImageNotFound("i".into()),
            RuntimeError::ConfigNotFound("c".into()),
            RuntimeError::InsufficientResources("m".into()),
            RuntimeError::PortAlreadyInUse(1),
            RuntimeError::VmStartFailed("boot".into()),
        ];
        for err in &errors {
            let mut deployment = vm_deployment();
            apply_vm_start_failure(
                &mut deployment,
                err,
                "firecracker",
                DeploymentStatus::CrashLoopBackOff,
            );
            assert!(
                deployment.restart_count > 0 || scheduler_skips_by_status(&deployment.status),
                "{:?} left the counter at zero AND landed on {:?}, which the scheduler \
                 still reconciles — the deployment would retry forever",
                err,
                deployment.status
            );
        }
    }

    /// A permanent failure lands on its terminal status immediately, so the very
    /// next tick is terminal instead of the fifth.
    ///
    /// `Failed` is outside the scheduler's reconcile filter, so the status alone
    /// stops the retries and the restart budget is left untouched — the
    /// deployment never restarted, and must not claim it did.
    #[test]
    fn terminal_start_failure_lands_on_its_status_at_once() {
        let mut deployment = vm_deployment();
        apply_vm_start_failure(
            &mut deployment,
            &RuntimeError::FirmwareNotFound("/no/vmlinux".into()),
            "firecracker",
            DeploymentStatus::CrashLoopBackOff,
        );

        assert_eq!(deployment.status, DeploymentStatus::Failed);
        assert_eq!(
            deployment.restart_count, 0,
            "the VM never started, so no restart may be reported"
        );
    }

    /// A transient failure keeps its one-per-tick budget and converges to the
    /// caller's bound status (CrashLoopBackOff for a worker, Failed for a job).
    #[test]
    fn transient_start_failure_converges_to_the_bound_status() {
        let mut deployment = vm_deployment();
        let max = crate::models::deployments::MAX_RESTART_COUNT;

        for tick in 1..max {
            apply_vm_start_failure(
                &mut deployment,
                &RuntimeError::VmStartFailed("boot timeout".into()),
                "firecracker",
                DeploymentStatus::CrashLoopBackOff,
            );
            assert_eq!(deployment.restart_count, tick);
            assert_ne!(deployment.status, DeploymentStatus::CrashLoopBackOff);
        }

        apply_vm_start_failure(
            &mut deployment,
            &RuntimeError::VmStartFailed("boot timeout".into()),
            "firecracker",
            DeploymentStatus::CrashLoopBackOff,
        );
        assert_eq!(deployment.restart_count, max);
        assert_eq!(deployment.status, DeploymentStatus::CrashLoopBackOff);
    }

    /// A job converges to Failed rather than CrashLoopBackOff.
    #[test]
    fn job_bound_status_is_honoured() {
        let mut deployment = vm_deployment();
        deployment.restart_count = crate::models::deployments::MAX_RESTART_COUNT - 1;

        apply_vm_start_failure(
            &mut deployment,
            &RuntimeError::VmStartFailed("boot timeout".into()),
            "cloud-hypervisor",
            DeploymentStatus::Failed,
        );

        assert_eq!(deployment.status, DeploymentStatus::Failed);
    }

    #[test]
    fn vm_start_insufficient_resources_is_terminal() {
        assert_eq!(
            classify_vm_start_error(&RuntimeError::InsufficientResources("need".into())),
            (
                Some(DeploymentStatus::InsufficientResources),
                "insufficient_resources"
            )
        );
    }

    #[test]
    fn other_exit_codes_retry() {
        // Generic failures and signal kills stay retryable (could be transient).
        assert_eq!(classify_exit_code(Some(1)), Disposition::Retry);
        assert_eq!(classify_exit_code(Some(137)), Disposition::Retry);
        assert_eq!(classify_exit_code(None), Disposition::Retry);
    }
}
