//! Per-VM resource stats shared by the KVM-backed runtimes (Cloud Hypervisor,
//! Firecracker).
//!
//! Docker exposes stats through the daemon's API; we have no equivalent for a
//! bare VMM process, so we read host-side files directly. Everything here keys
//! off the VMM process PID and the per-VM tap name, so it is VMM-agnostic:
//!
//! - **CPU**: two samples of `/proc/<pid>/stat` (utime + stime) divided by
//!   the elapsed wall time, normalised to 100% per online CPU.
//! - **Memory**: `VmRSS` from `/proc/<pid>/status`. With `MAP_SHARED` guest
//!   memory this captures both the VMM and the resident slice of guest RAM.
//! - **Network**: `/sys/class/net/<tap>/statistics/{rx,tx}_{bytes,packets}`.
//!   We swap host-side semantics to guest-side: host `tx` over the tap
//!   (bytes the host wrote toward the guest) maps to guest `rx`, matching
//!   the convention `ring deployment metrics` users expect from Docker.
//! - **Disk I/O**: `read_bytes` / `write_bytes` from `/proc/<pid>/io` when
//!   we can read it. A hardened VMM (e.g. Cloud Hypervisor) clears
//!   `PR_SET_DUMPABLE` early for sandboxing, which makes that file return
//!   `EACCES` even to the parent under `kernel.yama.ptrace_scope >= 1`. When
//!   unreadable, we report zeros — counted by host alone, with no other
//!   reliable source short of cgroup `io.stat` (which Ring does not currently
//!   set up per-VM).
//! - **PIDs**: `Threads:` from `/proc/<pid>/status` (vCPU threads + io +
//!   control). There is no cgroup pids.max per VM, so `limit` is reported as
//!   0 (= unlimited) to mirror Docker semantics.

use crate::api::dto::stats::{DiskIoStats, MemoryStats, NetworkStats, PidStats};

/// Two samples of `/proc/<pid>/stat` (utime + stime in clock ticks) and the
/// elapsed wall time between them. Combined, they give a CPU percentage.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CpuSample {
    /// Sum of `utime` and `stime` from `/proc/<pid>/stat` (fields 14 + 15).
    pub total_ticks: u64,
}

/// Parse `utime + stime` (fields 14 and 15) from a `/proc/<pid>/stat` line.
///
/// The 2nd field — `comm` — is parenthesised and may itself contain spaces,
/// so we split on the *last* `)` instead of trusting whitespace tokenisation.
pub(crate) fn parse_proc_stat(content: &str) -> Option<CpuSample> {
    let rparen = content.rfind(')')?;
    let after = content.get(rparen + 1..)?;
    let fields: Vec<&str> = after.split_whitespace().collect();
    // After `comm`, field 3 is `state`. utime is field 14 overall, i.e.
    // index 11 here (14 - 3 = 11).
    let utime: u64 = fields.get(11)?.parse().ok()?;
    let stime: u64 = fields.get(12)?.parse().ok()?;
    Some(CpuSample {
        total_ticks: utime.saturating_add(stime),
    })
}

/// Parse `VmRSS` (kB) from `/proc/<pid>/status` and return it in bytes.
pub(crate) fn parse_vm_rss_bytes(status: &str) -> u64 {
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            // Format: `VmRSS:    12345 kB`
            let kb: u64 = rest
                .split_whitespace()
                .next()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            return kb.saturating_mul(1024);
        }
    }
    0
}

/// CPU% over the interval, normalised the same way Docker reports it:
/// 100% per online CPU, so an N-vCPU VM saturating all cores reads as
/// `N * 100`.
pub(crate) fn compute_cpu_percent(
    prev: CpuSample,
    curr: CpuSample,
    interval_secs: f64,
    clock_ticks_per_sec: f64,
) -> f64 {
    if interval_secs <= 0.0 || clock_ticks_per_sec <= 0.0 {
        return 0.0;
    }
    let delta_ticks = curr.total_ticks.saturating_sub(prev.total_ticks) as f64;
    let cpu_seconds = delta_ticks / clock_ticks_per_sec;
    (cpu_seconds / interval_secs) * 100.0
}

pub(crate) fn memory_stats(usage_bytes: u64, limit_bytes: u64) -> MemoryStats {
    let usage_percent = if limit_bytes > 0 {
        (usage_bytes as f64 / limit_bytes as f64) * 100.0
    } else {
        0.0
    };
    MemoryStats {
        usage_bytes,
        limit_bytes,
        usage_percent,
    }
}

/// Read the four byte/packet counters under `/sys/class/net/<tap>/statistics/`
/// and return them as guest-side stats. Mapping: host `tx` (what the host
/// wrote to the tap, i.e. what the guest receives) → guest `rx`; host `rx`
/// (what the host read off the tap, i.e. what the guest sent) → guest `tx`.
/// Missing tap or unreadable counter → 0 for that field.
pub(crate) async fn network_stats_from_tap(tap_name: &str) -> NetworkStats {
    let base = format!("/sys/class/net/{}/statistics", tap_name);
    NetworkStats {
        rx_bytes: read_counter(&format!("{}/tx_bytes", base)).await,
        tx_bytes: read_counter(&format!("{}/rx_bytes", base)).await,
        rx_packets: read_counter(&format!("{}/tx_packets", base)).await,
        tx_packets: read_counter(&format!("{}/rx_packets", base)).await,
    }
}

async fn read_counter(path: &str) -> u64 {
    match tokio::fs::read_to_string(path).await {
        Ok(s) => s.trim().parse().unwrap_or(0),
        Err(_) => 0,
    }
}

/// Parse `/proc/<pid>/io` and return `(read_bytes, write_bytes)`. The kernel
/// fields are line-prefixed (`read_bytes: 12345`); anything missing yields 0.
pub(crate) fn parse_proc_io(content: &str) -> DiskIoStats {
    let mut read_bytes = 0u64;
    let mut write_bytes = 0u64;
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("read_bytes:") {
            read_bytes = rest.trim().parse().unwrap_or(0);
        } else if let Some(rest) = line.strip_prefix("write_bytes:") {
            write_bytes = rest.trim().parse().unwrap_or(0);
        }
    }
    DiskIoStats {
        read_bytes,
        write_bytes,
    }
}

pub(crate) async fn disk_io_stats(pid: u32) -> DiskIoStats {
    let path = format!("/proc/{}/io", pid);
    match tokio::fs::read_to_string(&path).await {
        Ok(s) => parse_proc_io(&s),
        Err(e) => {
            tracing::debug!(
                "disk_io_stats: could not read {} ({}); reporting zeros",
                path,
                e
            );
            DiskIoStats {
                read_bytes: 0,
                write_bytes: 0,
            }
        }
    }
}

/// Parse `Threads:` from `/proc/<pid>/status`. CH has no pid cap, so `limit`
/// stays at 0 (Docker convention for "unlimited").
pub(crate) fn parse_threads(status: &str) -> u64 {
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("Threads:") {
            return rest.trim().parse().unwrap_or(0);
        }
    }
    0
}

pub(crate) async fn pid_stats(pid: u32) -> PidStats {
    let path = format!("/proc/{}/status", pid);
    let current = match tokio::fs::read_to_string(&path).await {
        Ok(s) => parse_threads(&s),
        Err(_) => 0,
    };
    PidStats { current, limit: 0 }
}

/// Read `/proc/<pid>/stat` and parse it. Returns `None` if the process is
/// gone (typical race: VM was stopped between `list_instances` and the stats
/// call) or the file is malformed.
pub(crate) async fn read_cpu_sample(pid: u32) -> Option<CpuSample> {
    let path = format!("/proc/{}/stat", pid);
    let content = tokio::fs::read_to_string(&path).await.ok()?;
    parse_proc_stat(&content)
}

pub(crate) async fn read_rss_bytes(pid: u32) -> u64 {
    let path = format!("/proc/{}/status", pid);
    match tokio::fs::read_to_string(&path).await {
        Ok(s) => parse_vm_rss_bytes(&s),
        Err(_) => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_proc_stat_simple_comm() {
        let line =
            "1234 (cloud-hypervis) S 1 1234 1234 0 -1 4194304 100 0 0 0 250 750 0 0 20 0 4 0 12345";
        let sample = parse_proc_stat(line).expect("should parse");
        assert_eq!(sample.total_ticks, 1000);
    }

    #[test]
    fn parse_proc_stat_comm_with_spaces_and_parens() {
        let line = "42 (foo bar (baz)) R 1 42 42 0 -1 4194304 0 0 0 0 11 22 0 0 20 0 1 0 9";
        let sample = parse_proc_stat(line).expect("should parse despite tricky comm");
        assert_eq!(sample.total_ticks, 33);
    }

    #[test]
    fn parse_proc_stat_truncated_returns_none() {
        let line = "1 (short) S 1 1";
        assert!(parse_proc_stat(line).is_none());
    }

    #[test]
    fn parse_vm_rss_typical() {
        let status = "Name:\tch\nState:\tS (sleeping)\nVmRSS:\t  524288 kB\nThreads:\t8\n";
        assert_eq!(parse_vm_rss_bytes(status), 524288 * 1024);
    }

    #[test]
    fn parse_vm_rss_missing_returns_zero() {
        let status = "Name:\tch\nState:\tS (sleeping)\nThreads:\t8\n";
        assert_eq!(parse_vm_rss_bytes(status), 0);
    }

    #[test]
    fn cpu_percent_one_full_cpu_for_one_second() {
        // 100 ticks/sec, 100-tick delta, 1 second elapsed → 100%
        let prev = CpuSample { total_ticks: 1000 };
        let curr = CpuSample { total_ticks: 1100 };
        let pct = compute_cpu_percent(prev, curr, 1.0, 100.0);
        assert!((pct - 100.0).abs() < 0.001, "got {}", pct);
    }

    #[test]
    fn cpu_percent_two_cores_saturated() {
        // 200 ticks over 1s at 100Hz → 200% (matches Docker semantics)
        let prev = CpuSample { total_ticks: 0 };
        let curr = CpuSample { total_ticks: 200 };
        let pct = compute_cpu_percent(prev, curr, 1.0, 100.0);
        assert!((pct - 200.0).abs() < 0.001);
    }

    #[test]
    fn cpu_percent_zero_interval_returns_zero() {
        let prev = CpuSample { total_ticks: 0 };
        let curr = CpuSample { total_ticks: 100 };
        assert_eq!(compute_cpu_percent(prev, curr, 0.0, 100.0), 0.0);
    }

    #[test]
    fn cpu_percent_clock_decreases_returns_zero() {
        // Counter wraparound or process restart: never report negative.
        let prev = CpuSample { total_ticks: 1000 };
        let curr = CpuSample { total_ticks: 500 };
        assert_eq!(compute_cpu_percent(prev, curr, 1.0, 100.0), 0.0);
    }

    #[test]
    fn memory_stats_computes_percent() {
        let m = memory_stats(50 * 1024 * 1024, 100 * 1024 * 1024);
        assert!((m.usage_percent - 50.0).abs() < 0.001);
    }

    #[test]
    fn memory_stats_zero_limit_yields_zero_percent() {
        let m = memory_stats(50 * 1024 * 1024, 0);
        assert_eq!(m.usage_percent, 0.0);
    }

    #[test]
    fn parse_proc_io_typical() {
        let content = "rchar: 12345\nwchar: 6789\nsyscr: 100\nsyscw: 50\nread_bytes: 4096\nwrite_bytes: 8192\ncancelled_write_bytes: 0\n";
        let io = parse_proc_io(content);
        assert_eq!(io.read_bytes, 4096);
        assert_eq!(io.write_bytes, 8192);
    }

    #[test]
    fn parse_proc_io_missing_fields_yields_zero() {
        let content = "rchar: 12345\nwchar: 6789\n";
        let io = parse_proc_io(content);
        assert_eq!(io.read_bytes, 0);
        assert_eq!(io.write_bytes, 0);
    }

    #[test]
    fn parse_proc_io_malformed_lines_dont_panic() {
        let content = "read_bytes: not-a-number\nwrite_bytes:\n";
        let io = parse_proc_io(content);
        assert_eq!(io.read_bytes, 0);
        assert_eq!(io.write_bytes, 0);
    }

    #[test]
    fn parse_threads_typical() {
        let status = "Name:\tch\nState:\tS (sleeping)\nThreads:\t8\nVmRSS:\t  524288 kB\n";
        assert_eq!(parse_threads(status), 8);
    }

    #[test]
    fn parse_threads_missing_returns_zero() {
        let status = "Name:\tch\nState:\tS (sleeping)\n";
        assert_eq!(parse_threads(status), 0);
    }

    #[tokio::test]
    async fn network_stats_missing_tap_yields_zeros() {
        let stats = network_stats_from_tap("ring-test-nonexistent-tap-xyz").await;
        assert_eq!(stats.rx_bytes, 0);
        assert_eq!(stats.tx_bytes, 0);
        assert_eq!(stats.rx_packets, 0);
        assert_eq!(stats.tx_packets, 0);
    }
}
