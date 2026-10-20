//! Exit statuses of the VMM processes Ring spawned (Cloud Hypervisor,
//! Firecracker), recorded as each process exits.
//!
//! A VM runtime reports an instance whose VMM is gone as terminated, and this
//! is where it finds how the process ended. Kept in memory only: the instances
//! a `ring server` inherited from an earlier one are not watched, and the
//! runtime has to probe them instead.

use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VmExit {
    /// `None` when the process could not be waited on.
    pub(crate) exit_code: Option<i64>,
    pub(crate) finished_at: DateTime<Utc>,
}

/// A VMM process this server spawned and waits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Watched {
    Running,
    Exited(VmExit),
}

#[derive(Clone, Default)]
pub(crate) struct VmExits(Arc<Mutex<HashMap<String, Watched>>>);

impl VmExits {
    /// Mark the process of `instance_id` as spawned, before waiting on it.
    pub(crate) fn watch(&self, instance_id: &str) {
        if let Ok(mut map) = self.0.lock() {
            map.insert(instance_id.to_string(), Watched::Running);
        }
    }

    pub(crate) fn record(&self, instance_id: &str, status: std::io::Result<ExitStatus>) {
        let exit = VmExit {
            exit_code: status.ok().and_then(exit_code),
            finished_at: Utc::now(),
        };
        if let Ok(mut map) = self.0.lock() {
            map.insert(instance_id.to_string(), Watched::Exited(exit));
        }
    }

    /// `None` for a process this server did not spawn.
    pub(crate) fn get(&self, instance_id: &str) -> Option<Watched> {
        self.0.lock().ok()?.get(instance_id).copied()
    }

    /// Drop the exits of a deployment's instances that no longer exist. A VMM
    /// stopped by Ring exits after its instance is removed, and nothing else
    /// would ever read its exit.
    pub(crate) fn retain_known(&self, deployment_prefix: &str, known: &[String]) {
        if let Ok(mut map) = self.0.lock() {
            map.retain(|id, _| !id.starts_with(deployment_prefix) || known.contains(id));
        }
    }
}

/// The exit code of a process, shell style: `128 + n` for one killed by
/// signal `n`.
pub(crate) fn exit_code(status: ExitStatus) -> Option<i64> {
    status
        .code()
        .map(i64::from)
        .or_else(|| status.signal().map(|sig| 128 + i64::from(sig)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_signal_reads_as_128_plus_its_number() {
        assert_eq!(exit_code(ExitStatus::from_raw(9)), Some(137));
    }

    #[test]
    fn an_exit_code_reads_as_itself() {
        assert_eq!(exit_code(ExitStatus::from_raw(3 << 8)), Some(3));
        assert_eq!(exit_code(ExitStatus::from_raw(0)), Some(0));
    }

    #[test]
    fn a_watched_process_runs_until_its_exit_is_recorded() {
        let exits = VmExits::default();
        exits.watch("d1-a");
        assert_eq!(exits.get("d1-a"), Some(Watched::Running));
        exits.record("d1-a", Ok(ExitStatus::from_raw(0)));
        assert!(matches!(
            exits.get("d1-a"),
            Some(Watched::Exited(VmExit {
                exit_code: Some(0),
                ..
            }))
        ));
        assert_eq!(exits.get("d1-b"), None, "never spawned here");
    }

    #[test]
    fn exits_of_removed_instances_are_dropped() {
        let exits = VmExits::default();
        exits.record("d1-a", Ok(ExitStatus::from_raw(0)));
        exits.record("d1-b", Ok(ExitStatus::from_raw(0)));
        exits.record("d2-a", Ok(ExitStatus::from_raw(0)));
        exits.retain_known("d1-", &["d1-a".to_string()]);
        assert!(exits.get("d1-a").is_some());
        assert!(exits.get("d1-b").is_none());
        assert!(
            exits.get("d2-a").is_some(),
            "another deployment's exits are kept"
        );
    }
}
