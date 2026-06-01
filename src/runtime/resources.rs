//! Best-effort host-memory admission control, shared by both runtimes.
//!
//! Ring otherwise creates instances blindly: the scheduler keeps booting until
//! `current_count == target_count`, never looking at whether the host can
//! actually hold them. The failure mode is ugly and runtime-specific — Docker
//! lets the container start and the cgroup OOM-killer reaps it later (or, with
//! no limit, the *host* OOM-killer fires at random); Cloud Hypervisor fails the
//! VM spawn with an opaque `Cannot allocate memory` and crash-loops. Neither
//! tells the operator "you asked for more memory than this host has".
//!
//! This module adds one preventive check at boot: compare the deployment's
//! requested memory against the host's currently-available memory. It is
//! deliberately best-effort (a point-in-time `sysinfo` read, no reservation
//! accounting) — that matches the chosen scope. CPU is intentionally *not*
//! gated: CPU overcommit is a legitimate, non-fatal practice (the kernel just
//! time-slices), whereas memory overcommit is what actually crashes workloads.

use crate::models::deployments::{Deployment, parse_memory_string};
use crate::runtime::error::RuntimeError;
use sysinfo::System;

/// The memory figure we admit against, in bytes. `requests.memory` is the
/// scheduling intent ("how much this workload needs to run"), so it wins;
/// `limits.memory` is the cap and is used as a fallback when no request is
/// declared. Returns `None` when the deployment declares neither — in that
/// case we don't gate at all (we can't admit against a number we don't have).
pub(crate) fn required_memory_bytes(deployment: &Deployment) -> Option<i64> {
    let resources = deployment.resources.as_ref()?;

    let from = |spec: Option<&crate::models::deployments::ResourceSpec>| {
        spec.and_then(|s| s.memory.as_ref())
            .and_then(|m| parse_memory_string(m).ok())
    };

    from(resources.requests.as_ref()).or_else(|| from(resources.limits.as_ref()))
}

/// Pure comparison, split out so it can be unit-tested without touching the
/// host. `Ok(())` when there's enough headroom; otherwise an
/// `InsufficientResources` error whose message names both figures in MiB so the
/// operator immediately sees the gap.
pub(crate) fn check_against(
    deployment_name: &str,
    required_bytes: i64,
    available_bytes: i64,
) -> Result<(), RuntimeError> {
    if required_bytes <= available_bytes {
        return Ok(());
    }

    let mib = |b: i64| b / (1024 * 1024);
    Err(RuntimeError::InsufficientResources(format!(
        "insufficient host memory for '{}': needs {} MiB but only {} MiB is available — \
         free memory on the host or lower resources.requests.memory / resources.limits.memory",
        deployment_name,
        mib(required_bytes),
        mib(available_bytes),
    )))
}

/// Admit `deployment` against the host's currently-available memory. A no-op
/// when the deployment declares no memory request/limit. Best-effort: the
/// `available_memory` read is a snapshot and may race with other workloads
/// starting, but it catches the common, gross case ("asked for 8 GiB on a
/// 2 GiB host") before any expensive image pull or VM boot.
pub(crate) fn check_host_memory(deployment: &Deployment) -> Result<(), RuntimeError> {
    let Some(required) = required_memory_bytes(deployment) else {
        return Ok(());
    };

    let mut sys = System::new();
    sys.refresh_memory();
    // sysinfo reports bytes; `available_memory` accounts for reclaimable cache,
    // matching what the kernel would actually hand out.
    let available = sys.available_memory() as i64;

    check_against(&deployment.name, required, available)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::deployments::{Resource, ResourceSpec};

    fn bare_deployment() -> Deployment {
        Deployment {
            id: "dep-0001".to_string(),
            created_at: String::new(),
            updated_at: None,
            status: crate::models::deployments::DeploymentStatus::Pending,
            restart_count: 0,
            namespace: "default".to_string(),
            name: "web".to_string(),
            image: "alpine:3".to_string(),
            config: None,
            runtime: "docker".to_string(),
            kind: "worker".to_string(),
            replicas: 1,
            command: vec![],
            instances: vec![],
            labels: std::collections::HashMap::new(),
            environment: std::collections::HashMap::new(),
            volumes: "[]".to_string(),
            health_checks: vec![],
            resources: None,
            image_digest: None,
            ports: vec![],
            pending_events: vec![],
            parent_id: None,
            network: None,
        }
    }

    fn deployment_with(requests: Option<&str>, limits: Option<&str>) -> Deployment {
        let mut d = bare_deployment();
        d.resources = Some(Resource {
            requests: requests.map(|m| ResourceSpec {
                cpu: None,
                memory: Some(m.to_string()),
            }),
            limits: limits.map(|m| ResourceSpec {
                cpu: None,
                memory: Some(m.to_string()),
            }),
        });
        d
    }

    #[test]
    fn required_prefers_requests_over_limits() {
        let d = deployment_with(Some("256Mi"), Some("512Mi"));
        assert_eq!(required_memory_bytes(&d), Some(256 * 1024 * 1024));
    }

    #[test]
    fn required_falls_back_to_limits_when_no_request() {
        let d = deployment_with(None, Some("512Mi"));
        assert_eq!(required_memory_bytes(&d), Some(512 * 1024 * 1024));
    }

    #[test]
    fn required_is_none_when_nothing_declared() {
        let d = deployment_with(None, None);
        assert_eq!(required_memory_bytes(&d), None);

        // No `resources` block at all behaves the same.
        let bare = bare_deployment();
        assert_eq!(required_memory_bytes(&bare), None);
    }

    #[test]
    fn check_passes_when_within_headroom() {
        assert!(check_against("web", 200 * 1024 * 1024, 1024 * 1024 * 1024).is_ok());
        // Exactly equal is admitted.
        assert!(check_against("web", 512 * 1024 * 1024, 512 * 1024 * 1024).is_ok());
    }

    #[test]
    fn check_fails_and_names_both_figures() {
        let err = check_against("web", 8 * 1024 * 1024 * 1024, 2 * 1024 * 1024 * 1024)
            .expect_err("8 GiB on 2 GiB available must be refused");
        match err {
            RuntimeError::InsufficientResources(msg) => {
                assert!(msg.contains("'web'"), "names the deployment: {msg}");
                assert!(msg.contains("8192 MiB"), "names the requirement: {msg}");
                assert!(msg.contains("2048 MiB"), "names what's available: {msg}");
            }
            other => panic!("expected InsufficientResources, got {other:?}"),
        }
    }
}
