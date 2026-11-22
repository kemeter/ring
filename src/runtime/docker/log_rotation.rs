//! Log rotation for the containers Ring creates.
//!
//! Docker's default `json-file` driver keeps a container's whole output in a
//! single file and never rotates it unless told to. A container that writes a
//! lot fills the host's disk, and everything else on it (Ring's own database
//! included) starts failing. So Ring sets `max-size` / `max-file` on every
//! container it creates, mirroring the console log rotation of the microVM
//! runtimes.
//!
//! Only some drivers accept those options: Docker refuses to create a
//! container with `max-size` under `journald` or `fluentd`. Ring therefore
//! asks the daemon which driver it uses and leaves the logs alone when it is
//! not one that rotates files. Podman's Docker-compatible `/info` does not
//! report its driver; it applies `max-size` to `k8s-file` and ignores it under
//! `journald`, so an unreported driver gets the options too.

use bollard::Docker;
use bollard::models::HostConfigLogConfig;
use std::collections::HashMap;
use tracing::{info, warn};

pub(crate) const DEFAULT_MAX_LOG_BYTES: u64 = 10 * 1024 * 1024;
pub(crate) const DEFAULT_MAX_LOG_FILES: u32 = 3;

/// Drivers that write rotating files and accept `max-size` / `max-file`.
const ROTATING_DRIVERS: &[&str] = &["json-file", "local"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LogRotation {
    pub(crate) max_bytes: u64,
    pub(crate) max_files: u32,
}

impl LogRotation {
    /// The `HostConfig.LogConfig` to send. The driver itself is left unset so
    /// the daemon keeps its own default; only its options are given.
    pub(crate) fn log_config(&self) -> HostConfigLogConfig {
        HostConfigLogConfig {
            typ: None,
            config: Some(HashMap::from([
                ("max-size".to_string(), self.max_bytes.to_string()),
                ("max-file".to_string(), self.max_files.max(1).to_string()),
            ])),
        }
    }
}

/// Pure decision, split out to be tested without a daemon. `driver` is what
/// the daemon reported (`None` or empty when it reported nothing).
pub(crate) fn decide(max_bytes: u64, max_files: u32, driver: Option<&str>) -> Option<LogRotation> {
    if max_bytes == 0 {
        return None;
    }
    match driver.filter(|d| !d.is_empty()) {
        Some(d) if !ROTATING_DRIVERS.contains(&d) => None,
        _ => Some(LogRotation {
            max_bytes,
            max_files,
        }),
    }
}

/// Resolve the rotation for a runtime at startup. `runtime` names it in logs.
pub(crate) async fn resolve(
    docker: &Docker,
    runtime: &str,
    max_bytes: Option<u64>,
    max_files: Option<u32>,
) -> Option<LogRotation> {
    let max_bytes = max_bytes.unwrap_or(DEFAULT_MAX_LOG_BYTES);
    let max_files = max_files.unwrap_or(DEFAULT_MAX_LOG_FILES);
    if max_bytes == 0 {
        info!(
            "{runtime}: log rotation disabled (max_log_bytes = 0), the daemon's logging settings apply"
        );
        return None;
    }

    let driver = match docker.info().await {
        Ok(info) => info.logging_driver,
        Err(e) => {
            warn!(
                "{runtime}: could not read the daemon's logging driver ({e}), rotating container logs anyway"
            );
            None
        }
    };

    let rotation = decide(max_bytes, max_files, driver.as_deref());
    match (&rotation, driver.as_deref()) {
        (None, Some(d)) => info!(
            "{runtime}: the daemon logs with '{d}', which does not rotate files; Ring leaves container logs to it"
        ),
        (Some(r), _) => info!(
            "{runtime}: container logs rotate at {} bytes, {} files kept",
            r.max_bytes, r.max_files
        ),
        (None, None) => {}
    }
    rotation
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotating_drivers_get_the_options() {
        for driver in ["json-file", "local"] {
            assert_eq!(
                decide(1024, 2, Some(driver)),
                Some(LogRotation {
                    max_bytes: 1024,
                    max_files: 2
                })
            );
        }
    }

    #[test]
    fn other_drivers_are_left_alone() {
        // Docker refuses `max-size` for these: sending it would fail every create.
        for driver in ["journald", "fluentd", "syslog", "none"] {
            assert_eq!(decide(1024, 2, Some(driver)), None);
        }
    }

    #[test]
    fn unreported_driver_gets_the_options() {
        // Podman's compat `/info` reports no driver.
        assert!(decide(1024, 2, None).is_some());
        assert!(decide(1024, 2, Some("")).is_some());
    }

    #[test]
    fn zero_bytes_disables_rotation() {
        assert_eq!(decide(0, 3, Some("json-file")), None);
    }

    #[test]
    fn log_config_keeps_the_daemon_driver() {
        let config = LogRotation {
            max_bytes: 10_485_760,
            max_files: 3,
        }
        .log_config();
        assert_eq!(config.typ, None);
        let opts = config.config.unwrap();
        assert_eq!(opts["max-size"], "10485760");
        assert_eq!(opts["max-file"], "3");
    }

    #[test]
    fn max_files_is_at_least_one() {
        let opts = LogRotation {
            max_bytes: 1024,
            max_files: 0,
        }
        .log_config()
        .config
        .unwrap();
        assert_eq!(opts["max-file"], "1");
    }
}
