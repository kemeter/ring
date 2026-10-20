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
//! A permanent problem (image that doesn't exist, missing config, a binary that
//! isn't executable) must not look like a transient one: this classifier folds
//! the permanent-vs-transient decision into one place, so such a failure lands
//! on its own reason at once. The restart policy still retries it, from the
//! longest wait, since only an operator can fix it.
//!
//! It is deliberately runtime-agnostic (it only reads the shared
//! [`RuntimeError`] enum and a raw exit code), and the scheduler's reconcile
//! pass applies it the same way to every runtime.

use crate::hypervisor::error::RuntimeError;
use crate::models::deployments::Reason;

/// What to do with a failed deployment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Disposition {
    /// Permanent failure, for this reason. A retry right away would only fail
    /// the same way.
    Terminal(Reason),
    /// Transient failure: retry on the backoff curve.
    Retry,
}

/// Classify a create-boundary [`RuntimeError`] into terminal-or-retry while
/// preserving the reason each error maps to (see [`create_error_outcome`]).
///
/// Terminal: the failure can't fix itself on a retry — the image truly doesn't
/// exist, the config/key is absent, the container spec is rejected, firmware is
/// missing, or the host is out of memory. Retry: registry/network hiccups, a
/// busy port, a VM that failed to boot, and other transient/unknown errors.
pub(crate) fn classify_create_error(err: &RuntimeError) -> Disposition {
    match err {
        // Permanent: the image isn't there (or policy forbids pulling it).
        RuntimeError::ImageNotFound(_) => Disposition::Terminal(Reason::ImagePull),
        // Transient: a pull that failed mid-flight (registry/network) can succeed
        // on a retry.
        RuntimeError::ImagePullFailed(_) => Disposition::Retry,
        // Permanent: the runtime rejecting `create`/`start` almost always means
        // a bad container spec (entrypoint, mount, options) — retrying
        // re-submits the same rejected spec. Fail fast onto spec_rejected.
        RuntimeError::InstanceCreationFailed(_) => Disposition::Terminal(Reason::SpecRejected),
        // Transient: network setup can race with daemon/host state.
        RuntimeError::NetworkCreationFailed(_) => Disposition::Retry,
        // Permanent: the referenced config (or key) is absent — a retry won't
        // conjure it. The operator must create it.
        RuntimeError::ConfigNotFound(_) | RuntimeError::ConfigKeyNotFound(_) => {
            Disposition::Terminal(Reason::ConfigMissing)
        }
        // Permanent: firmware/kernel file missing at the configured path.
        RuntimeError::FirmwareNotFound(_) => Disposition::Terminal(Reason::Error),
        // The host is short on memory now: its own reason, but memory is freed
        // over time, so the restart policy treats it as transient.
        RuntimeError::InsufficientResources(_) => {
            Disposition::Terminal(Reason::InsufficientResources)
        }
        // Transient: another process holds the port; it may be released.
        RuntimeError::PortAlreadyInUse(_) => Disposition::Retry,
        // Transient/unknown: worth a retry.
        RuntimeError::VmStartFailed(_)
        | RuntimeError::StatsFetchFailed(_)
        | RuntimeError::Other(_)
        | RuntimeError::Io(_)
        | RuntimeError::Json(_) => Disposition::Retry,
    }
}

/// The reason, event reason and operator-facing message a create-boundary
/// [`RuntimeError`] maps to. Shared by the runtimes and the scheduler so the
/// same failure reads the same everywhere.
pub(crate) fn create_error_outcome(
    err: &RuntimeError,
    deployment: &crate::models::deployments::Deployment,
) -> (Reason, &'static str, String) {
    match err {
        RuntimeError::ImageNotFound(detail) => (
            Reason::ImagePull,
            "image_pull_back_off",
            // Preserve the inner detail — it carries operator-relevant
            // context like `image_pull_policy=Never forbids pulling` that a
            // generic "not found" would drop.
            format!("Image '{}' not found: {}", deployment.image, detail),
        ),
        RuntimeError::ImagePullFailed(detail) => (
            Reason::ImagePull,
            "image_pull_back_off",
            format!("Failed to pull image '{}': {}", deployment.image, detail),
        ),
        RuntimeError::InstanceCreationFailed(msg) => (
            Reason::SpecRejected,
            "instance_creation_failed",
            format!("Container creation failed: {}", msg),
        ),
        RuntimeError::NetworkCreationFailed(_) => (
            Reason::Network,
            "network_creation_failed",
            format!(
                "Failed to create network for namespace '{}'",
                deployment.namespace
            ),
        ),
        RuntimeError::ConfigNotFound(_) => (
            Reason::ConfigMissing,
            "config_error",
            format!("Config not found in namespace '{}'", deployment.namespace),
        ),
        RuntimeError::ConfigKeyNotFound(_) => (
            Reason::ConfigMissing,
            "config_error",
            format!(
                "Config key not found in namespace '{}'",
                deployment.namespace
            ),
        ),
        RuntimeError::StatsFetchFailed(msg) => (
            Reason::Error,
            "stats_fetch_failed",
            format!("Stats fetch failed: {}", msg),
        ),
        RuntimeError::InsufficientResources(detail) => (
            Reason::InsufficientResources,
            "insufficient_resources",
            detail.clone(),
        ),
        RuntimeError::Other(msg) => (
            Reason::Error,
            "runtime_error",
            format!("Runtime error: {}", msg),
        ),
        // CH-specific variants — Docker should never produce them, but the
        // enum is shared so we map them to the closest Docker-side state.
        RuntimeError::FirmwareNotFound(msg) => (
            Reason::Error,
            "firmware_not_found",
            format!("Firmware not found: {}", msg),
        ),
        RuntimeError::VmStartFailed(msg) => (
            Reason::Error,
            "vm_start_failed",
            format!("VM start failed: {}", msg),
        ),
        // The CH runtime emits this; the Docker runtime doesn't (the daemon
        // rejects port conflicts itself through InstanceCreationFailed). The
        // arm exists to keep the match exhaustive over the shared enum.
        RuntimeError::PortAlreadyInUse(port) => (
            Reason::Error,
            "port_allocation_failed",
            format!("Port {} is already allocated", port),
        ),
        RuntimeError::Io(e) => (
            Reason::FileSystem,
            "file_system_error",
            format!("IO error: {}", e),
        ),
        RuntimeError::Json(e) => (Reason::Error, "runtime_error", format!("JSON error: {}", e)),
    }
}

/// Classify a crash-boundary exit code into terminal-or-retry.
///
/// A worker that exits is normally restarted (transient): the process may have
/// hit a one-off error and a fresh start can recover. Two exit codes are the
/// exception because they are *unambiguously* permanent under the standard shell
/// convention, so a restart can never succeed:
///
/// * `127` — command not found (the entrypoint/binary doesn't exist);
/// * `126` — found but not executable (bad perms / not a binary).
///
/// 126/127 mean the container can never start its program, so it lands on
/// `spec_rejected` at once and waits the longest backoff. Every
/// other code (generic `1`, signal-kill `128+n`) stays retryable — those can be
/// transient, and mislabelling them terminal would wrongly give up on a
/// recoverable worker.
///
/// A clean exit (`0`) is retryable too. A worker is a long-running service by
/// definition, so exiting is never "done": a service that shuts down cleanly on
/// SIGTERM (a host reboot, a Docker daemon restart, a manual stop) exits 0 and
/// must come back. Only jobs can be `Completed`. A one-shot program declared as
/// a worker is a misconfiguration; it is recreated like any exited worker, on
/// the backoff curve.
pub(crate) fn classify_exit_code(exit_code: Option<i64>) -> Disposition {
    match exit_code {
        Some(126) | Some(127) => Disposition::Terminal(Reason::SpecRejected),
        _ => Disposition::Retry,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_not_found_is_terminal_image_pull() {
        let d = classify_create_error(&RuntimeError::ImageNotFound("x".into()));
        assert_eq!(d, Disposition::Terminal(Reason::ImagePull));
    }

    #[test]
    fn image_pull_failed_is_retry() {
        assert_eq!(
            classify_create_error(&RuntimeError::ImagePullFailed("net".into())),
            Disposition::Retry
        );
    }

    #[test]
    fn instance_creation_failed_is_terminal_spec_rejected() {
        assert_eq!(
            classify_create_error(&RuntimeError::InstanceCreationFailed("bad".into())),
            Disposition::Terminal(Reason::SpecRejected)
        );
    }

    #[test]
    fn config_errors_are_terminal() {
        assert_eq!(
            classify_create_error(&RuntimeError::ConfigNotFound("c".into())),
            Disposition::Terminal(Reason::ConfigMissing)
        );
        assert_eq!(
            classify_create_error(&RuntimeError::ConfigKeyNotFound("k".into())),
            Disposition::Terminal(Reason::ConfigMissing)
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
            Disposition::Terminal(Reason::InsufficientResources)
        );
    }

    #[test]
    fn unexecutable_and_missing_command_exit_codes_are_terminal() {
        assert_eq!(
            classify_exit_code(Some(126)),
            Disposition::Terminal(Reason::SpecRejected)
        );
        assert_eq!(
            classify_exit_code(Some(127)),
            Disposition::Terminal(Reason::SpecRejected)
        );
    }

    #[test]
    fn clean_exit_retries() {
        // A worker that exits 0 (e.g. a graceful shutdown on SIGTERM during a
        // host reboot) must be recreated, never treated as finished.
        assert_eq!(classify_exit_code(Some(0)), Disposition::Retry);
    }

    #[test]
    fn other_exit_codes_retry() {
        // Generic failures and signal kills stay retryable (could be transient).
        assert_eq!(classify_exit_code(Some(1)), Disposition::Retry);
        assert_eq!(classify_exit_code(Some(137)), Disposition::Retry);
        assert_eq!(classify_exit_code(None), Disposition::Retry);
    }
}
