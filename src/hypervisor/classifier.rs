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
//! and a raw exit code) so containerd and the VM runtimes can adopt it next; for
//! now only the Docker runtime is wired to it.

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
    fn other_exit_codes_retry() {
        // Generic failures and signal kills stay retryable (could be transient).
        assert_eq!(classify_exit_code(Some(1)), Disposition::Retry);
        assert_eq!(classify_exit_code(Some(137)), Disposition::Retry);
        assert_eq!(classify_exit_code(None), Disposition::Retry);
    }
}
