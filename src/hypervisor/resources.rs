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
//! deliberately best-effort (a point-in-time `sysinfo` read) — that matches
//! the chosen scope. CPU is intentionally *not*
//! gated: CPU overcommit is a legitimate, non-fatal practice (the kernel just
//! time-slices), whereas memory overcommit is what actually crashes workloads.

//!
//! A process takes a while to allocate what it asked for. Admitted against the
//! available memory alone, a second replica started before the first one has
//! allocated sees the same free memory and is admitted too, and the OOM killer
//! picks between them later. So each admission reserves the memory it was
//! admitted for, for [`RESERVATION_TTL`], and the next admissions subtract the
//! reservations still running. Near the limit an instance that already
//! allocated counts twice until its reservation expires; the refused instance
//! is retried on the backoff curve, which beats an OOM.

use crate::hypervisor::error::RuntimeError;
use crate::models::deployments::{Deployment, parse_memory_string};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use sysinfo::System;
use tracing::warn;

/// How long an admitted instance's memory stays reserved: long enough for a
/// process to allocate what it asked for, after which it shows in the host's
/// available memory.
pub(crate) const RESERVATION_TTL: Duration = Duration::from_secs(60);

/// Memory admitted recently, host-wide (every runtime admits against the same
/// host), keyed by deployment.
#[derive(Default)]
pub(crate) struct Reservations {
    entries: Vec<Reservation>,
}

struct Reservation {
    deployment_id: String,
    at: Instant,
    bytes: i64,
}

impl Reservations {
    /// Bytes still reserved at `now`, dropping the expired entries.
    pub(crate) fn reserved(&mut self, now: Instant) -> i64 {
        self.entries
            .retain(|r| now.saturating_duration_since(r.at) < RESERVATION_TTL);
        self.entries.iter().map(|r| r.bytes).sum()
    }

    /// Reserve `bytes` for one instance of `deployment_id`. A deployment never
    /// has more than `replicas` instances alive, so it never holds more than
    /// `replicas` reservations: the oldest goes, which is what keeps a crash
    /// loop restarting the same instance from stacking reservations.
    pub(crate) fn reserve(&mut self, now: Instant, deployment_id: &str, replicas: u32, bytes: i64) {
        let held = self
            .entries
            .iter()
            .filter(|r| r.deployment_id == deployment_id)
            .count();
        if held >= replicas.max(1) as usize
            && let Some(oldest) = self
                .entries
                .iter()
                .position(|r| r.deployment_id == deployment_id)
        {
            self.entries.remove(oldest);
        }
        self.entries.push(Reservation {
            deployment_id: deployment_id.to_string(),
            at: now,
            bytes,
        });
    }

    /// Admit `required_bytes` against `available_bytes` minus what is still
    /// reserved, and reserve it when admitted.
    pub(crate) fn admit(
        &mut self,
        now: Instant,
        deployment: &Deployment,
        required_bytes: i64,
        available_bytes: i64,
    ) -> Result<(), RuntimeError> {
        let replicas = deployment.replicas;
        // The oldest reservation of this deployment is about to make room for
        // this one when it already holds all its slots: do not count it.
        let reserved = self.reserved(now) - self.displaced(deployment, replicas);
        check_against(&deployment.name, required_bytes, available_bytes, reserved)?;
        self.reserve(now, &deployment.id, replicas, required_bytes);
        Ok(())
    }

    fn displaced(&self, deployment: &Deployment, replicas: u32) -> i64 {
        let mut own = self
            .entries
            .iter()
            .filter(|r| r.deployment_id == deployment.id);
        if own.clone().count() >= replicas.max(1) as usize {
            own.next().map(|r| r.bytes).unwrap_or(0)
        } else {
            0
        }
    }
}

static RESERVATIONS: Mutex<Reservations> = Mutex::new(Reservations {
    entries: Vec::new(),
});

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
/// host, minus what instances admitted moments ago still reserve. `Ok(())`
/// when there's enough headroom; otherwise an `InsufficientResources` error
/// whose message names the figures in MiB so the operator immediately sees the
/// gap.
pub(crate) fn check_against(
    deployment_name: &str,
    required_bytes: i64,
    available_bytes: i64,
    reserved_bytes: i64,
) -> Result<(), RuntimeError> {
    let headroom = available_bytes - reserved_bytes;
    if required_bytes <= headroom {
        return Ok(());
    }

    let mib = |b: i64| b / (1024 * 1024);
    let available = if reserved_bytes > 0 {
        format!(
            "only {} MiB is available ({} MiB free, {} MiB reserved by instances started in the last {}s)",
            mib(headroom.max(0)),
            mib(available_bytes),
            mib(reserved_bytes),
            RESERVATION_TTL.as_secs(),
        )
    } else {
        format!("only {} MiB is available", mib(available_bytes))
    };
    Err(RuntimeError::InsufficientResources(format!(
        "insufficient host memory for '{}': needs {} MiB but {} — \
         free memory on the host or lower resources.requests.memory / resources.limits.memory",
        deployment_name,
        mib(required_bytes),
        available,
    )))
}

/// Admit `deployment` against the host's currently-available memory, minus
/// the memory reserved by the instances admitted in the last
/// [`RESERVATION_TTL`], and reserve its own on success. A no-op when the
/// deployment declares no memory request/limit. Best-effort: the
/// `available_memory` read is a snapshot and may race with workloads Ring did
/// not start, but it catches the common, gross case ("asked for 8 GiB on a
/// 2 GiB host") and replicas outrunning each other before any expensive image
/// pull or VM boot.
pub(crate) fn check_host_memory(deployment: &Deployment) -> Result<(), RuntimeError> {
    let Some(required) = required_memory_bytes(deployment) else {
        return Ok(());
    };

    let mut sys = match System::new() {
        Ok(sys) => sys,
        Err(e) => {
            warn!(
                "skipping the host memory check for '{}': {}",
                deployment.name, e
            );
            return Ok(());
        }
    };
    sys.refresh_memory();
    // sysinfo reports bytes; `available_memory` accounts for reclaimable cache,
    // matching what the kernel would actually hand out.
    let available = sys.available_memory() as i64;

    // A poisoned lock only means another admission panicked mid-way; the
    // ledger is still a plain list of reservations.
    let mut reservations = RESERVATIONS.lock().unwrap_or_else(|e| e.into_inner());
    reservations.admit(Instant::now(), deployment, required, available)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::deployments::{Resource, ResourceSpec};
    use std::collections::HashMap;

    fn bare_deployment() -> Deployment {
        Deployment {
            id: "dep-0001".to_string(),
            created_at: String::new(),
            updated_at: None,
            status: crate::models::deployments::DeploymentStatus::Pending,
            reason: None,
            message: None,
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
            pending_retire: vec![],
            waiting: None,
            parent_id: None,
            network: None,
            restart: None,
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
        assert!(check_against("web", 200 * 1024 * 1024, 1024 * 1024 * 1024, 0).is_ok());
        // Exactly equal is admitted.
        assert!(check_against("web", 512 * 1024 * 1024, 512 * 1024 * 1024, 0).is_ok());
    }

    #[test]
    fn check_fails_and_names_both_figures() {
        let err = check_against("web", 8 * 1024 * 1024 * 1024, 2 * 1024 * 1024 * 1024, 0)
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

    const MIB: i64 = 1024 * 1024;

    fn replicated(id: &str, replicas: u32) -> Deployment {
        let mut d = bare_deployment();
        d.id = id.to_string();
        d.name = id.to_string();
        d.replicas = replicas;
        d
    }

    #[test]
    fn a_second_replica_cannot_reuse_memory_the_first_was_admitted_for() {
        // 575 MiB free, two replicas of 300 MiB: the first one has not
        // allocated yet when the second is admitted, so the host still shows
        // 575 MiB free. Only the reservation tells them apart.
        let mut ledger = Reservations::default();
        let t0 = Instant::now();
        let d = replicated("memhog", 3);
        assert!(ledger.admit(t0, &d, 300 * MIB, 575 * MIB).is_ok());
        let second = ledger.admit(t0 + Duration::from_secs(5), &d, 300 * MIB, 575 * MIB);
        match second {
            Err(RuntimeError::InsufficientResources(msg)) => {
                assert!(msg.contains("only 275 MiB is available"), "{msg}");
                assert!(msg.contains("300 MiB reserved"), "{msg}");
            }
            other => panic!("expected InsufficientResources, got {other:?}"),
        }
    }

    #[test]
    fn replicas_that_fit_together_are_all_admitted() {
        let mut ledger = Reservations::default();
        let t0 = Instant::now();
        let d = replicated("web", 3);
        for i in 0..3 {
            assert!(
                ledger
                    .admit(t0 + Duration::from_secs(i), &d, 300 * MIB, 1600 * MIB)
                    .is_ok()
            );
        }
        assert_eq!(ledger.reserved(t0 + Duration::from_secs(3)), 900 * MIB);
    }

    #[test]
    fn a_reservation_expires_after_its_ttl() {
        let mut ledger = Reservations::default();
        let t0 = Instant::now();
        ledger.reserve(t0, "web", 1, 300 * MIB);
        assert_eq!(
            ledger.reserved(t0 + RESERVATION_TTL - Duration::from_secs(1)),
            300 * MIB
        );
        assert_eq!(ledger.reserved(t0 + RESERVATION_TTL), 0);
        // Once expired, the memory counts only through what the host reports.
        assert!(
            ledger
                .admit(
                    t0 + RESERVATION_TTL,
                    &replicated("other", 1),
                    300 * MIB,
                    300 * MIB
                )
                .is_ok()
        );
    }

    #[test]
    fn a_refused_admission_reserves_nothing() {
        let mut ledger = Reservations::default();
        let t0 = Instant::now();
        assert!(
            ledger
                .admit(t0, &replicated("big", 1), 800 * MIB, 500 * MIB)
                .is_err()
        );
        assert_eq!(ledger.reserved(t0), 0);
    }

    #[test]
    fn a_crash_loop_does_not_stack_reservations() {
        // replicas: 1 restarted five times in a few seconds holds one
        // reservation, not five, so it cannot starve other deployments.
        let mut ledger = Reservations::default();
        let t0 = Instant::now();
        let d = replicated("crashy", 1);
        for i in 0..5 {
            assert!(
                ledger
                    .admit(t0 + Duration::from_secs(i), &d, 300 * MIB, 400 * MIB)
                    .is_ok(),
                "restart #{i} replaces its own reservation"
            );
        }
        assert_eq!(ledger.reserved(t0 + Duration::from_secs(5)), 300 * MIB);
        assert!(
            ledger
                .admit(
                    t0 + Duration::from_secs(5),
                    &replicated("other", 1),
                    100 * MIB,
                    400 * MIB
                )
                .is_ok()
        );
    }
}
