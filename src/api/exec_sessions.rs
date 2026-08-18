//! Admission and lifetime bounds for interactive `exec` sessions.
//!
//! This module exists because of a hole in the Docker API: an exec, once
//! started, cannot be killed. There is no delete endpoint, and dropping the
//! hijacked connection leaves the process running inside the container. A
//! client that disconnects therefore leaks a process for as long as the
//! container lives.
//!
//! Ring cannot close that hole, so it bounds it instead:
//!
//! * a global slot count, so exec cannot exhaust the daemon;
//! * a hard ceiling on how long any one session may run;
//! * an idle timeout, which is what actually catches the leak above, since a
//!   vanished client stops producing traffic long before it stops existing.
//!
//! Nothing here terminates a process. The honest framing is that these
//! limits bound *Ring's participation* in a session: past them Ring stops
//! relaying and releases the slot. A command that ignores its closed stdin
//! keeps running until the container stops, and no amount of bookkeeping on
//! our side changes that.

use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::config::server::ExecConfig;

/// Why a session could not be admitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AdmissionError {
    /// Exec is switched off in the daemon's config.
    Disabled,
    /// Every concurrent slot is taken.
    AtCapacity,
}

/// Admission control for exec sessions.
///
/// Cloneable, and every clone shares the same slot pool: the limit is a
/// property of the daemon, not of whoever happens to hold a handle.
#[derive(Clone)]
pub(crate) struct ExecSessionLimiter {
    enabled: bool,
    slots: Arc<Semaphore>,
    max_duration: Duration,
    idle_timeout: Duration,
}

/// A held slot. Dropping it returns the slot to the pool, so a session that
/// ends any way at all — clean exit, disconnect, timeout, panic — frees its
/// capacity without anyone remembering to say so.
#[derive(Debug)]
pub(crate) struct SessionSlot {
    _permit: OwnedSemaphorePermit,
    max_duration: Duration,
    idle_timeout: Duration,
}

impl SessionSlot {
    /// Hard ceiling for this session.
    pub(crate) fn max_duration(&self) -> Duration {
        self.max_duration
    }

    /// How long the session may sit silent before being disconnected.
    pub(crate) fn idle_timeout(&self) -> Duration {
        self.idle_timeout
    }
}

impl ExecSessionLimiter {
    pub(crate) fn new(config: &ExecConfig) -> Self {
        // A zero limit would deadlock every request on a semaphore that can
        // never be acquired, which reads at runtime like a hang rather than
        // like the misconfiguration it is. Treat it as "disabled", which is
        // what an operator writing 0 means.
        let permits = config.max_concurrent_sessions.max(1);
        Self {
            enabled: config.enabled && config.max_concurrent_sessions > 0,
            slots: Arc::new(Semaphore::new(permits)),
            max_duration: Duration::from_secs(config.max_duration_seconds),
            idle_timeout: Duration::from_secs(config.idle_timeout_seconds),
        }
    }

    /// Take a slot, or explain why not.
    ///
    /// Deliberately non-blocking: a caller that queued would hold a
    /// WebSocket open showing nothing, and an operator watching a full
    /// daemon would see connections pile up rather than a clear refusal.
    pub(crate) fn admit(&self) -> Result<SessionSlot, AdmissionError> {
        if !self.enabled {
            return Err(AdmissionError::Disabled);
        }

        match self.slots.clone().try_acquire_owned() {
            Ok(permit) => Ok(SessionSlot {
                _permit: permit,
                max_duration: self.max_duration,
                idle_timeout: self.idle_timeout,
            }),
            Err(_) => Err(AdmissionError::AtCapacity),
        }
    }

    /// Whether exec is switched on at all. Lets a handler answer 501 before
    /// doing any work — resolving a deployment, reaching a runtime — on a
    /// request it will refuse anyway.
    pub(crate) fn is_enabled(&self) -> bool {
        self.enabled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(enabled: bool, max_sessions: usize) -> ExecConfig {
        ExecConfig {
            enabled,
            max_duration_seconds: 60,
            idle_timeout_seconds: 30,
            max_concurrent_sessions: max_sessions,
        }
    }

    #[test]
    fn disabled_by_default_refuses_everything() {
        let limiter = ExecSessionLimiter::new(&ExecConfig::default());
        assert!(!limiter.is_enabled());
        assert_eq!(limiter.admit().unwrap_err(), AdmissionError::Disabled);
    }

    #[test]
    fn admits_up_to_the_configured_limit() {
        let limiter = ExecSessionLimiter::new(&config(true, 2));
        let _a = limiter.admit().expect("first session");
        let _b = limiter.admit().expect("second session");
        assert_eq!(limiter.admit().unwrap_err(), AdmissionError::AtCapacity);
    }

    #[test]
    fn dropping_a_session_frees_its_slot() {
        // The leak this guards against: a session that ends by disconnect
        // rather than by clean exit must still return its capacity, or the
        // daemon bleeds slots until it refuses every exec.
        let limiter = ExecSessionLimiter::new(&config(true, 1));
        let first = limiter.admit().expect("first session");
        assert_eq!(limiter.admit().unwrap_err(), AdmissionError::AtCapacity);
        drop(first);
        assert!(limiter.admit().is_ok(), "slot was not returned on drop");
    }

    #[test]
    fn clones_share_one_pool() {
        // Each WebSocket handler gets its own clone from axum state; if the
        // pool were per-clone the limit would be meaningless.
        let limiter = ExecSessionLimiter::new(&config(true, 1));
        let other = limiter.clone();
        let _held = limiter.admit().expect("first session");
        assert_eq!(other.admit().unwrap_err(), AdmissionError::AtCapacity);
    }

    #[test]
    fn zero_limit_reads_as_disabled_not_as_a_hang() {
        let limiter = ExecSessionLimiter::new(&config(true, 0));
        assert!(!limiter.is_enabled());
        assert_eq!(limiter.admit().unwrap_err(), AdmissionError::Disabled);
    }

    #[test]
    fn slot_carries_the_configured_deadlines() {
        let limiter = ExecSessionLimiter::new(&config(true, 1));
        let slot = limiter.admit().expect("session");
        assert_eq!(slot.max_duration(), Duration::from_secs(60));
        assert_eq!(slot.idle_timeout(), Duration::from_secs(30));
    }
}
