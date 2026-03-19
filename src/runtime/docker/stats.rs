use bollard::Docker;
use bollard::models::ContainerStatsResponse;
use bollard::query_parameters::StatsOptionsBuilder;
use futures::StreamExt;

use crate::api::dto::stats::*;
use crate::runtime::error::RuntimeError;

pub(crate) async fn fetch_container_stats(
    docker: &Docker,
    container_id: &str,
) -> Result<ContainerStatsResponse, RuntimeError> {
    let options = StatsOptionsBuilder::new()
        .stream(false)
        .one_shot(true)
        .build();

    let mut stream = docker.stats(container_id, Some(options));

    match stream.next().await {
        Some(Ok(stats)) => Ok(stats),
        Some(Err(e)) => Err(RuntimeError::StatsFetchFailed(format!(
            "Failed to get stats for {}: {}",
            container_id, e
        ))),
        None => Err(RuntimeError::StatsFetchFailed(format!(
            "No stats returned for container {}",
            container_id
        ))),
    }
}

pub(crate) async fn fetch_restart_count(docker: &Docker, container_id: &str) -> u64 {
    match docker
        .inspect_container(
            container_id,
            None::<bollard::query_parameters::InspectContainerOptions>,
        )
        .await
    {
        Ok(info) => info.restart_count.unwrap_or(0) as u64,
        Err(_) => 0,
    }
}

pub(crate) fn compute_cpu_percent(stats: &ContainerStatsResponse) -> f64 {
    let cpu_stats = match &stats.cpu_stats {
        Some(c) => c,
        None => return 0.0,
    };
    let precpu_stats = match &stats.precpu_stats {
        Some(c) => c,
        None => return 0.0,
    };

    let cpu_delta = cpu_stats
        .cpu_usage
        .as_ref()
        .and_then(|u| u.total_usage)
        .unwrap_or(0) as f64
        - precpu_stats
            .cpu_usage
            .as_ref()
            .and_then(|u| u.total_usage)
            .unwrap_or(0) as f64;

    let system_delta = cpu_stats.system_cpu_usage.unwrap_or(0) as f64
        - precpu_stats.system_cpu_usage.unwrap_or(0) as f64;

    let online_cpus = cpu_stats.online_cpus.unwrap_or(1) as f64;

    if system_delta > 0.0 && cpu_delta >= 0.0 {
        (cpu_delta / system_delta) * online_cpus * 100.0
    } else {
        0.0
    }
}

pub(crate) fn compute_memory_stats(stats: &ContainerStatsResponse) -> MemoryStats {
    let mem = stats.memory_stats.as_ref();
    let usage = mem.and_then(|m| m.usage).unwrap_or(0);
    let limit = mem.and_then(|m| m.limit).unwrap_or(0);
    let cache = mem
        .and_then(|m| m.stats.as_ref())
        .and_then(|s| s.get("cache").copied())
        .unwrap_or(0);
    let actual_usage = usage.saturating_sub(cache);

    MemoryStats {
        usage_bytes: actual_usage,
        limit_bytes: limit,
        usage_percent: if limit > 0 {
            (actual_usage as f64 / limit as f64) * 100.0
        } else {
            0.0
        },
    }
}

pub(crate) fn compute_network_stats(stats: &ContainerStatsResponse) -> NetworkStats {
    let mut rx_bytes = 0u64;
    let mut tx_bytes = 0u64;
    let mut rx_packets = 0u64;
    let mut tx_packets = 0u64;

    if let Some(networks) = &stats.networks {
        for net in networks.values() {
            rx_bytes += net.rx_bytes.unwrap_or(0);
            tx_bytes += net.tx_bytes.unwrap_or(0);
            rx_packets += net.rx_packets.unwrap_or(0);
            tx_packets += net.tx_packets.unwrap_or(0);
        }
    }

    NetworkStats {
        rx_bytes,
        tx_bytes,
        rx_packets,
        tx_packets,
    }
}

pub(crate) fn compute_disk_io_stats(stats: &ContainerStatsResponse) -> DiskIoStats {
    let mut read_bytes = 0u64;
    let mut write_bytes = 0u64;

    if let Some(blkio) = &stats.blkio_stats
        && let Some(entries) = &blkio.io_service_bytes_recursive
    {
        for entry in entries {
            match entry.op.as_deref() {
                Some("read") | Some("Read") => read_bytes += entry.value.unwrap_or(0),
                Some("write") | Some("Write") => write_bytes += entry.value.unwrap_or(0),
                _ => {}
            }
        }
    }

    DiskIoStats {
        read_bytes,
        write_bytes,
    }
}

pub(crate) fn compute_pid_stats(stats: &ContainerStatsResponse) -> PidStats {
    let pids = stats.pids_stats.as_ref();
    PidStats {
        current: pids.and_then(|p| p.current).unwrap_or(0),
        limit: pids.and_then(|p| p.limit).unwrap_or(0),
    }
}
