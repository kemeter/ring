use crate::api::dto::stats::InstanceStatsOutput;
use crate::models::deployments::Deployment;
use crate::models::health_check::{HealthCheck, HealthCheckStatus};
use crate::models::volume::ResolvedMount;
use async_trait::async_trait;
use axum::response::sse::Event;
use futures::stream;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::convert::Infallible;
use std::net::IpAddr;
use std::pin::Pin;
use std::sync::LazyLock;

#[derive(Clone, Deserialize, Serialize, Debug)]
pub(crate) struct Log {
    pub(crate) instance: String,
    pub(crate) message: String,
    pub(crate) level: String,
    pub(crate) timestamp: Option<String>,
}

/// Best-effort log level classification. Recognises three families of
/// conventions that show up in a Ring stream:
///
/// 1. **Web app** — nginx/Apache-style bracketed markers (`[error]`,
///    `[warning]`, `[notice]`, `[info]`, or `info:` prefixes).
/// 2. **Linux kernel** — the `<N>` syslog priority prefix (0..=3 → error,
///    4 → warning, 5..=6 → info, 7 → debug) and crash markers (`BUG:`,
///    `oops:`, `panic:`, `Kernel panic`).
/// 3. **systemd / cloud-init / generic CLI** — uppercase level words like
///    `ERROR`, `ERR`, `CRITICAL`, `WARN`, `WARNING`, `NOTICE`, `INFO`,
///    `DEBUG` as they appear in cloud-init log lines and systemd journal
///    output piped to the console.
///
/// Unrecognised content falls back to `"unknown"`. Match order goes from
/// the most specific to the most generic so a kernel `<3>` priority
/// doesn't get demoted to `info` by a stray `INFO` appearing later in the
/// same line.
pub(crate) fn classify_log(log: &str) -> String {
    // Web app conventions (existing behaviour — keep first to preserve
    // backwards compat with anything that already depended on it).
    if log.contains("[error]") {
        return "error".to_string();
    }
    if log.contains("[warning]") {
        return "warning".to_string();
    }
    if log.contains("[notice]") || log.contains("[info]") || log.contains("info:") {
        return "info".to_string();
    }

    // Kernel crash markers are unambiguous — match before generic words.
    if log.contains("Kernel panic")
        || log.contains("BUG:")
        || log.contains("Oops:")
        || log.contains("oops:")
        || log.contains("panic:")
    {
        return "error".to_string();
    }

    // Kernel syslog priority prefix `<N>` at the start of the line (or after
    // a leading timestamp like `[    0.123456] <3>...`). Priority 0-3 maps
    // to error, 4 to warning, 5-6 to info, 7 to debug — matches RFC 5424.
    if let Some(level) = parse_syslog_priority(log) {
        return level.to_string();
    }

    // Bracketed uppercase markers (e.g. `hypervisor-fw` boot log uses
    // `[INFO]`, `[WARN]`, `[ERROR]`).
    if log.contains("[ERROR]") || log.contains("[CRITICAL]") {
        return "error".to_string();
    }
    if log.contains("[WARNING]") || log.contains("[WARN]") {
        return "warning".to_string();
    }
    if log.contains("[NOTICE]") || log.contains("[INFO]") {
        return "info".to_string();
    }
    if log.contains("[DEBUG]") {
        return "debug".to_string();
    }

    // systemd / cloud-init / generic uppercase markers. Word-boundary check
    // by surrounding spaces/punctuation so `INFORMATION` doesn't match `INFO`.
    for (needle, lvl) in [
        ("ERROR", "error"),
        ("CRITICAL", "error"),
        (" ERR ", "error"),
        ("WARNING", "warning"),
        (" WARN ", "warning"),
        ("NOTICE", "info"),
        (" INFO ", "info"),
        ("INFO:", "info"),
        (" DEBUG ", "debug"),
        ("DEBUG:", "debug"),
    ] {
        if log.contains(needle) {
            return lvl.to_string();
        }
    }

    "unknown".to_string()
}

/// Map the leading `<N>` syslog priority of a kernel/systemd line to a
/// Ring level. Tolerates an optional leading kernel timestamp `[<secs>]`.
fn parse_syslog_priority(log: &str) -> Option<&'static str> {
    let trimmed = log.trim_start();
    // Optional `[ time ] ` prefix from kernel logs (kmsg console).
    let after_ts = if let Some(rest) = trimmed.strip_prefix('[') {
        match rest.find(']') {
            Some(idx) => rest[idx + 1..].trim_start(),
            None => trimmed,
        }
    } else {
        trimmed
    };
    let rest = after_ts.strip_prefix('<')?;
    let close = rest.find('>')?;
    let digits = &rest[..close];
    if digits.len() != 1 {
        return None;
    }
    match digits.chars().next()? {
        '0' | '1' | '2' | '3' => Some("error"),
        '4' => Some("warning"),
        '5' | '6' => Some("info"),
        '7' => Some("debug"),
        _ => None,
    }
}

static DATE_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\d{4}/\d{2}/\d{2} \d{2}:\d{2}:\d{2}").unwrap());

pub(crate) fn extract_date(log: &str) -> Option<String> {
    let date = DATE_REGEX.find(log).map(|d| d.as_str()).unwrap_or("");
    if date.is_empty() {
        return None;
    }
    Some(date.to_string())
}

#[async_trait]
pub(crate) trait RuntimeLifecycle: Send + Sync {
    async fn apply(
        &self,
        deployment: Deployment,
        resolved_mounts: Vec<ResolvedMount>,
    ) -> Deployment;

    async fn list_instances(&self, deployment_id: String, status: &str) -> Vec<String>;

    /// Resolve the running instance ids for many deployments at once, keyed by
    /// deployment id.
    ///
    /// The default implementation loops over `list_instances` per deployment,
    /// preserving the previous behaviour for runtimes with no cheaper bulk path.
    /// Container runtimes (Docker) override this to issue a single host-wide
    /// list and group in memory, turning an N-deployments fan-out into one call
    /// — this is what keeps `GET /deployments` from timing out on busy hosts.
    ///
    /// Only ids are returned (no address): the deployment listing needs the
    /// instance *count*, and resolving an address is a separate per-instance
    /// inspect that callers opt into when they actually need it.
    async fn list_running_instances_grouped(
        &self,
        deployment_ids: &[String],
    ) -> HashMap<String, Vec<String>> {
        let mut grouped = HashMap::new();
        for deployment_id in deployment_ids {
            let ids = self.list_instances(deployment_id.clone(), "running").await;
            grouped.insert(deployment_id.clone(), ids);
        }
        grouped
    }

    /// Fallback: uses instance ID as display name. Override for runtimes
    /// that assign human-readable names (e.g. Docker container names).
    async fn list_instances_with_names(
        &self,
        deployment_id: String,
        status: &str,
    ) -> Vec<(String, String)> {
        self.list_instances(deployment_id, status)
            .await
            .into_iter()
            .map(|id| {
                let name = id.clone();
                (id, name)
            })
            .collect()
    }

    async fn remove_instance(&self, instance_id: String) -> bool;

    async fn get_logs(
        &self,
        _deployment_id: &str,
        _tail: Option<&str>,
        _since: Option<i32>,
        _instance_filter: Option<&str>,
    ) -> Vec<Log> {
        Vec::new()
    }

    async fn stream_logs(
        &self,
        _deployment_id: &str,
        _tail: Option<&str>,
        _since: Option<i32>,
        _instance_filter: Option<&str>,
    ) -> Pin<Box<dyn futures::Stream<Item = Result<Event, Infallible>> + Send>> {
        Box::pin(stream::empty())
    }

    /// Resolve the instance's reachable address for an external probe.
    ///
    /// Runtimes that expose their workloads on the host network or on a
    /// runtime-private network should override this so TCP/HTTP probes can
    /// reach the workload. The default returns `None`, which causes the
    /// default `execute_health_check` to fail any TCP/HTTP probe with a
    /// clear "could not resolve" message.
    async fn instance_address(&self, _instance_id: &str) -> Option<IpAddr> {
        None
    }

    /// Run a `command` health-check probe inside the instance.
    ///
    /// Container runtimes implement this via `docker exec` or equivalent.
    /// VM runtimes have no direct equivalent — supporting `command` requires
    /// an in-guest agent (vsock or SSH), so the default impl reports the
    /// limitation up front rather than silently appearing to work.
    async fn execute_command_probe(
        &self,
        _instance_id: &str,
        _command: &str,
    ) -> (HealthCheckStatus, Option<String>) {
        (
            HealthCheckStatus::Failed,
            Some("command health checks are not supported on this runtime".to_string()),
        )
    }

    /// Execute one health-check definition for one instance.
    ///
    /// The default impl orchestrates the three probe types via shared
    /// helpers: `tcp` and `http` probes go through `health_probes` once an
    /// IP has been resolved via [`Self::instance_address`]; `command`
    /// probes are delegated to [`Self::execute_command_probe`].
    ///
    /// A runtime overrides this only if it needs to deviate from the
    /// shared pipeline — for example, the Docker runtime keeps its own
    /// override because its IP-resolution path is interleaved with
    /// `bollard::inspect_container` calls that already exist there.
    async fn execute_health_check(
        &self,
        instance_id: &str,
        health_check: &HealthCheck,
    ) -> (HealthCheckStatus, Option<String>) {
        let timeout = match HealthCheck::parse_duration(health_check.timeout()) {
            Ok(d) => d,
            Err(e) => {
                return (
                    HealthCheckStatus::Failed,
                    Some(format!("Invalid timeout duration: {}", e)),
                );
            }
        };

        match health_check {
            HealthCheck::Tcp { port, .. } => {
                let Some(ip) = self.instance_address(instance_id).await else {
                    return (
                        HealthCheckStatus::Failed,
                        Some(format!(
                            "Could not resolve instance address for {}",
                            instance_id
                        )),
                    );
                };
                crate::hypervisor::health_probes::tcp_probe(ip, *port, timeout).await
            }
            HealthCheck::Http { url, .. } => {
                let Some(ip) = self.instance_address(instance_id).await else {
                    return (
                        HealthCheckStatus::Failed,
                        Some(format!(
                            "Could not resolve instance address for {}",
                            instance_id
                        )),
                    );
                };
                crate::hypervisor::health_probes::http_probe(ip, url, timeout).await
            }
            HealthCheck::Command { command, .. } => {
                self.execute_command_probe(instance_id, command).await
            }
        }
    }

    async fn get_instance_stats(&self, _deployment_id: &str) -> Vec<InstanceStatsOutput> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_log() {
        assert_eq!(classify_log("[info] This is an info log"), "info");
        assert_eq!(classify_log("[error] This is an error log"), "error");
        assert_eq!(classify_log("[warning] This is a warning log"), "warning");
        assert_eq!(classify_log("[notice] This is a notice log"), "info");
        assert_eq!(classify_log("info: This is a notice log"), "info");
        assert_eq!(classify_log("Coucou"), "unknown");
    }

    #[test]
    fn classify_kernel_syslog_priority() {
        // <3> = error
        assert_eq!(classify_log("<3>EXT4-fs (vda1): mounting failed"), "error");
        // <4> = warning
        assert_eq!(classify_log("<4>random: crng init done"), "warning");
        // <6> = info
        assert_eq!(classify_log("<6>Booting Linux on physical CPU 0x0"), "info");
        // <7> = debug
        assert_eq!(classify_log("<7>device-mapper: ioctl"), "debug");
    }

    #[test]
    fn classify_kernel_priority_after_timestamp() {
        // Kernel kmsg console: `[    1.234567] <3>message`
        assert_eq!(
            classify_log("[    1.234567] <3>EXT4-fs: mount error"),
            "error"
        );
    }

    #[test]
    fn classify_kernel_crash_markers() {
        assert_eq!(classify_log("Kernel panic - not syncing: VFS"), "error");
        assert_eq!(
            classify_log("BUG: unable to handle kernel paging request"),
            "error"
        );
        assert_eq!(classify_log("Oops: 0002 [#1] SMP"), "error");
    }

    #[test]
    fn classify_cloud_init_levels() {
        // cloud-init lines typically look like:
        // "Mon, 10 Jan 2024 12:34:56 +0000 - util.py[WARNING]: ..."
        // or "2024-01-10 12:34:56,123 - util.py [DEBUG] ..."
        assert_eq!(
            classify_log("util.py[WARNING]: hostname not set"),
            "warning"
        );
        assert_eq!(
            classify_log("cloud-init[123]: ERROR: failed to fetch data"),
            "error"
        );
        assert_eq!(
            classify_log("CRITICAL: cloud-init failed permanently"),
            "error"
        );
        assert_eq!(classify_log("INFO: handling cloud-config"), "info");
    }

    #[test]
    fn classify_systemd_journal_levels() {
        // systemd lines piped to console: `systemd[1]: Started <service>.`
        // not specially levelled — but service output often is:
        assert_eq!(classify_log("sshd: WARN sshguard active"), "warning");
        assert_eq!(classify_log("nginx ERROR upstream timeout"), "error");
        assert_eq!(
            classify_log("application started DEBUG flag enabled"),
            "debug"
        );
    }

    #[test]
    fn classify_priority_order_kernel_beats_lowercase() {
        // A kernel <3> at the start must dominate even if `INFO` appears
        // later in the same line.
        assert_eq!(
            classify_log("<3>BUG triggered, INFO: extra context follows"),
            "error"
        );
    }

    #[test]
    fn classify_bracketed_uppercase_levels() {
        // `hypervisor-fw` boot log produces lines like `[INFO] Page tables`.
        assert_eq!(
            classify_log("[INFO] Setting up 4 GiB identity mapping"),
            "info"
        );
        assert_eq!(classify_log("[WARN] Suboptimal config"), "warning");
        assert_eq!(classify_log("[ERROR] could not load file"), "error");
        assert_eq!(classify_log("[DEBUG] trace state"), "debug");
    }

    #[test]
    fn classify_information_does_not_match_info() {
        // Substring guard: `INFORMATION` must not classify as info — the
        // word-boundary spaces / colon around `INFO` matter.
        assert_eq!(classify_log("nothing INFORMATION here"), "unknown");
    }

    #[test]
    fn test_extract_date() {
        assert_eq!(
            extract_date("2021/08/10 12:00:00 [info] This is an info log"),
            Some("2021/08/10 12:00:00".to_string())
        );
        assert_eq!(extract_date("[info] This is an info log"), None);
    }
}
