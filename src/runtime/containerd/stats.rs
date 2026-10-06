//! Per-instance resource statistics via the Tasks `Metrics` RPC.
//!
//! `Tasks.Metrics` returns a `containerd.types.Metric` whose `data` is a
//! protobuf `Any` carrying the cgroup metrics — for cgroup v2 hosts the type is
//! `io.containerd.cgroups.v2.Metrics`, for v1 `io.containerd.cgroups.v1.Metrics`.
//! Those message definitions live in the containerd `cgroups` crate, which this
//! build does **not** depend on, so we cannot strongly-decode them here.
//!
//! Rather than pull in another proto crate, we decode the fields Ring reports
//! — CPU time, memory usage and pids — directly from the cgroup v2 `Metrics`
//! wire format using prost's field-level reader. CPU time is a counter, so the
//! percentage comes from two samples: each call keeps the latest one per
//! instance (see [`CpuSamples`]). Network and disk counters have no source in
//! the cgroup metrics and are reported as zero.

use crate::api::dto::stats::*;
use containerd_client::services::v1::MetricsRequest;
use containerd_client::services::v1::tasks_client::TasksClient;
use containerd_client::with_namespace;
use prost::bytes::Buf;
use prost::encoding::{DecodeContext, WireType, decode_key, skip_field};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tonic::Request;

/// The last CPU reading of each instance: its cumulated CPU time (µs) and
/// when it was read. Shared by the stats calls of one runtime, so the next
/// call can turn the counter into a percentage.
pub(crate) type CpuSamples = Arc<Mutex<HashMap<String, (u64, Instant)>>>;

/// How long to wait for a second reading when an instance has no earlier one.
/// Reporting 0% instead would read as an idle instance, and the autoscaler
/// acts on it.
const FIRST_SAMPLE_WINDOW: Duration = Duration::from_millis(250);

/// CPU used between two readings, in percent of one CPU (200 for two busy
/// cores), the definition Docker uses. A counter going backwards (a task
/// replaced under the same id) or no elapsed time gives 0.
fn cpu_percent(previous: (u64, Instant), current: (u64, Instant)) -> f64 {
    let elapsed = current.1.saturating_duration_since(previous.1).as_micros() as f64;
    if elapsed <= 0.0 || current.0 < previous.0 {
        return 0.0;
    }
    (current.0 - previous.0) as f64 / elapsed * 100.0
}

/// One reading of the task's cgroup metrics.
async fn read_metrics(
    client: &containerd_client::Client,
    namespace: &str,
    instance_id: &str,
) -> Option<CgroupSample> {
    let mut tasks = TasksClient::new(client.channel());
    let req = with_namespace!(
        MetricsRequest {
            filters: vec![format!("id=={}", instance_id)],
        },
        namespace
    );
    let resp = tasks.metrics(req).await.ok()?;
    let metric = resp.into_inner().metrics.into_iter().next()?;
    let data = metric.data?;
    Some(decode_metrics(&data.type_url, &data.value))
}

/// Decode a metrics payload, which only cgroup v2 hosts send in the layout
/// [`decode_cgroup_v2`] reads. A cgroup v1 payload numbers its fields
/// differently (field 2 is the process count there, not CPU), so reading it
/// with the v2 layout would report plausible but wrong values: it is left
/// empty instead.
fn decode_metrics(type_url: &str, value: &[u8]) -> CgroupSample {
    if type_url.ends_with("io.containerd.cgroups.v2.Metrics") {
        decode_cgroup_v2(value)
    } else {
        CgroupSample::default()
    }
}

/// Fetch and map the stats of `instances` (`(id, name)` pairs), omitting those
/// whose task has no metrics (not running).
///
/// The CPU percentage of an instance comes from its previous reading. When
/// some instances have none, they are read a second time after
/// [`FIRST_SAMPLE_WINDOW`] — once for the whole call, so a deployment with
/// many new instances still answers quickly.
pub(crate) async fn fetch_instances_stats(
    client: &containerd_client::Client,
    namespace: &str,
    instances: &[(String, String)],
    cpu_samples: &CpuSamples,
) -> Vec<InstanceStatsOutput> {
    let mut readings: Vec<(&String, &String, CgroupSample, Instant)> = read_all(
        client,
        namespace,
        instances.iter().map(|(id, _)| id).collect(),
    )
    .await
    .into_iter()
    .zip(instances.iter().map(|(_, name)| name))
    .filter_map(|((id, sample, at), name)| sample.map(|sample| (id, name, sample, at)))
    .collect();

    // Instances without an earlier reading are read a second time, after a
    // short wait shared by all of them.
    let known: Vec<String> = cpu_samples
        .lock()
        .map(|samples| samples.keys().cloned().collect())
        .unwrap_or_default();
    let mut first_readings: HashMap<String, (u64, Instant)> = HashMap::new();
    let first_seen: Vec<&String> = readings
        .iter()
        .filter(|(id, _, sample, _)| sample.cpu_usage_usec.is_some() && !known.contains(id))
        .map(|(id, _, sample, at)| {
            first_readings.insert(id.to_string(), (sample.cpu_usage_usec.unwrap_or(0), *at));
            *id
        })
        .collect();
    if !first_seen.is_empty() {
        tokio::time::sleep(FIRST_SAMPLE_WINDOW).await;
        for (id, second, at) in read_all(client, namespace, first_seen).await {
            if let (Some(second), Some(reading)) = (second, readings.iter_mut().find(|r| r.0 == id))
            {
                reading.2 = second;
                reading.3 = at;
            }
        }
    }

    readings
        .into_iter()
        .map(|(id, name, sample, at)| {
            let cpu_usage_percent = match sample.cpu_usage_usec {
                Some(usage) => record(cpu_samples, id, first_readings.get(id), (usage, at)),
                // A cgroup v1 host, or a reading without CPU: nothing to compute from.
                None => 0.0,
            };
            to_output(id, name, &sample, cpu_usage_percent)
        })
        .collect()
}

/// Read the metrics of every instance in `ids` at once, so a call takes about
/// as long as the slowest request rather than their sum.
async fn read_all<'a>(
    client: &containerd_client::Client,
    namespace: &str,
    ids: Vec<&'a String>,
) -> Vec<(&'a String, Option<CgroupSample>, Instant)> {
    futures::future::join_all(ids.into_iter().map(|id| async move {
        (
            id,
            read_metrics(client, namespace, id).await,
            Instant::now(),
        )
    }))
    .await
}

/// Compute an instance's CPU percentage from `reading` and make it the
/// instance's latest reading, under the lock so callers running at the same
/// time stay ordered: a reading older than the one already stored neither
/// replaces it nor is measured against it, which would give a false 0%.
fn record(
    cpu_samples: &CpuSamples,
    id: &str,
    first_reading: Option<&(u64, Instant)>,
    reading: (u64, Instant),
) -> f64 {
    let Ok(mut samples) = cpu_samples.lock() else {
        return first_reading.map_or(0.0, |&first| cpu_percent(first, reading));
    };
    match samples.get(id).copied() {
        Some(stored) if stored.1 >= reading.1 => {
            // Another caller already recorded a newer reading.
            first_reading.map_or(0.0, |&first| cpu_percent(first, reading))
        }
        stored => {
            samples.insert(id.to_string(), reading);
            match stored.or(first_reading.copied()) {
                Some(before) => cpu_percent(before, reading),
                None => 0.0,
            }
        }
    }
}

fn to_output(
    instance_id: &str,
    instance_name: &str,
    sample: &CgroupSample,
    cpu_usage_percent: f64,
) -> InstanceStatsOutput {
    let mem_usage = sample.memory_usage;
    let mem_limit = sample.memory_limit;
    InstanceStatsOutput {
        instance_id: instance_id.chars().take(12).collect(),
        instance_name: instance_name.to_string(),
        cpu_usage_percent,
        memory: MemoryStats {
            usage_bytes: mem_usage,
            limit_bytes: mem_limit,
            usage_percent: if mem_limit > 0 {
                (mem_usage as f64 / mem_limit as f64) * 100.0
            } else {
                0.0
            },
        },
        // cgroup metrics carry no per-interface network accounting.
        network: NetworkStats {
            rx_bytes: 0,
            tx_bytes: 0,
            rx_packets: 0,
            tx_packets: 0,
        },
        disk_io: DiskIoStats {
            read_bytes: 0,
            write_bytes: 0,
        },
        pids: PidStats {
            current: sample.pids_current,
            limit: sample.pids_limit,
        },
        // containerd tracks restarts via the shim, not the metrics RPC; Ring's
        // own restart_count on the deployment is authoritative, so 0 here.
        restart_count: 0,
    }
}

/// The fields Ring reads from one cgroup v2 `Metrics` message.
#[derive(Debug, Default, PartialEq)]
struct CgroupSample {
    /// Cumulated CPU time in µs; `None` when the message carries no CPU stat.
    cpu_usage_usec: Option<u64>,
    memory_usage: u64,
    memory_limit: u64,
    pids_current: u64,
    pids_limit: u64,
}

/// Best-effort field-level decode of an `io.containerd.cgroups.v2.Metrics`
/// message.
///
/// The v2 `Metrics` layout (from containerd's `cgroups/v2/stats.proto`):
///   field 1 = Pids   { current=1 (uint64), limit=2 (uint64) }
///   field 2 = CPU    { usage_usec=1 (uint64), ... }
///   field 4 = Memory { ... usage=32 (uint64), usage_limit=33 (uint64) ... }
///
/// We walk the top-level message, recurse into those submessages, and read the
/// specific tags. Unknown fields are skipped. Absent fields stay at zero (or
/// `None` for CPU), as on a cgroup v1 host, where the layout differs and
/// nothing matches.
fn decode_cgroup_v2(mut buf: &[u8]) -> CgroupSample {
    let mut sample = CgroupSample::default();

    while buf.has_remaining() {
        let Ok((tag, wire)) = decode_key(&mut buf) else {
            break;
        };
        match (tag, wire) {
            // Pids submessage (Metrics.pids, field 1).
            (1, WireType::LengthDelimited) => {
                if let Some(sub) = read_len_delimited(&mut buf) {
                    (sample.pids_current, sample.pids_limit) = decode_pids(sub);
                }
            }
            // CPU submessage (Metrics.cpu, field 2).
            (2, WireType::LengthDelimited) => {
                if let Some(sub) = read_len_delimited(&mut buf) {
                    sample.cpu_usage_usec = Some(decode_cpu(sub));
                }
            }
            // Memory submessage (Metrics.memory, field 4).
            (4, WireType::LengthDelimited) => {
                if let Some(sub) = read_len_delimited(&mut buf) {
                    (sample.memory_usage, sample.memory_limit) = decode_memory(sub);
                }
            }
            _ => {
                if skip_field(wire, tag, &mut buf, DecodeContext::default()).is_err() {
                    break;
                }
            }
        }
    }
    sample
}

/// `CPUStat.usage_usec` (field 1).
fn decode_cpu(mut buf: &[u8]) -> u64 {
    let mut usage = 0u64;
    while buf.has_remaining() {
        let Ok((tag, wire)) = decode_key(&mut buf) else {
            break;
        };
        match (tag, wire) {
            (1, WireType::Varint) => usage = read_varint(&mut buf),
            _ => {
                if skip_field(wire, tag, &mut buf, DecodeContext::default()).is_err() {
                    break;
                }
            }
        }
    }
    usage
}

fn decode_pids(mut buf: &[u8]) -> (u64, u64) {
    let mut current = 0u64;
    let mut limit = 0u64;
    while buf.has_remaining() {
        let Ok((tag, wire)) = decode_key(&mut buf) else {
            break;
        };
        match (tag, wire) {
            (1, WireType::Varint) => current = read_varint(&mut buf),
            (2, WireType::Varint) => limit = read_varint(&mut buf),
            _ => {
                if skip_field(wire, tag, &mut buf, DecodeContext::default()).is_err() {
                    break;
                }
            }
        }
    }
    (current, limit)
}

fn decode_memory(mut buf: &[u8]) -> (u64, u64) {
    let mut usage = 0u64;
    let mut limit = 0u64;
    while buf.has_remaining() {
        let Ok((tag, wire)) = decode_key(&mut buf) else {
            break;
        };
        match (tag, wire) {
            // MemoryStat.usage = field 32, usage_limit = field 33 in
            // io.containerd.cgroups.v2.MemoryStat (NOT 10/11 — those are
            // anon_thp / inactive_anon and would report bogus values).
            (32, WireType::Varint) => usage = read_varint(&mut buf),
            (33, WireType::Varint) => limit = read_varint(&mut buf),
            _ => {
                if skip_field(wire, tag, &mut buf, DecodeContext::default()).is_err() {
                    break;
                }
            }
        }
    }
    (usage, limit)
}

fn read_varint(buf: &mut &[u8]) -> u64 {
    prost::encoding::decode_varint(buf).unwrap_or(0)
}

fn read_len_delimited<'a>(buf: &mut &'a [u8]) -> Option<&'a [u8]> {
    let len = prost::encoding::decode_varint(buf).ok()? as usize;
    if buf.remaining() < len {
        return None;
    }
    let (head, tail) = buf.split_at(len);
    *buf = tail;
    Some(head)
}

#[cfg(test)]
mod tests {
    use super::*;
    use prost::encoding::{encode_key, encode_varint};

    fn encode_pids(current: u64, limit: u64) -> Vec<u8> {
        let mut b = Vec::new();
        encode_key(1, WireType::Varint, &mut b);
        encode_varint(current, &mut b);
        encode_key(2, WireType::Varint, &mut b);
        encode_varint(limit, &mut b);
        b
    }

    #[test]
    fn decode_pids_roundtrip() {
        let bytes = encode_pids(7, 100);
        assert_eq!(decode_pids(&bytes), (7, 100));
    }

    #[test]
    fn decode_top_level_pids_and_memory() {
        // Build a minimal v2 Metrics matching the real proto field numbers:
        // Metrics.pids = field 1, Metrics.memory = field 4; inside MemoryStat,
        // usage = field 32, usage_limit = field 33.
        let pids = encode_pids(3, 50);
        let mut mem = Vec::new();
        encode_key(32, WireType::Varint, &mut mem);
        encode_varint(2048, &mut mem);
        encode_key(33, WireType::Varint, &mut mem);
        encode_varint(4096, &mut mem);

        let mut top = Vec::new();
        encode_key(1, WireType::LengthDelimited, &mut top);
        encode_varint(pids.len() as u64, &mut top);
        top.extend_from_slice(&pids);
        encode_key(4, WireType::LengthDelimited, &mut top);
        encode_varint(mem.len() as u64, &mut top);
        top.extend_from_slice(&mem);

        let mut cpu = Vec::new();
        encode_key(1, WireType::Varint, &mut cpu);
        encode_varint(1_500_000, &mut cpu);
        encode_key(2, WireType::Varint, &mut cpu); // user_usec, ignored
        encode_varint(9, &mut cpu);
        encode_key(2, WireType::LengthDelimited, &mut top);
        encode_varint(cpu.len() as u64, &mut top);
        top.extend_from_slice(&cpu);

        assert_eq!(
            decode_cgroup_v2(&top),
            CgroupSample {
                cpu_usage_usec: Some(1_500_000),
                memory_usage: 2048,
                memory_limit: 4096,
                pids_current: 3,
                pids_limit: 50,
            }
        );
    }

    #[test]
    fn decode_empty_is_zeros() {
        assert_eq!(decode_cgroup_v2(&[]), CgroupSample::default());
    }

    #[test]
    fn cpu_percent_is_relative_to_one_cpu() {
        let start = Instant::now();
        let later = start + Duration::from_secs(1);
        // Half a CPU-second in one second.
        assert_eq!(cpu_percent((0, start), (500_000, later)), 50.0);
        // Two busy cores.
        assert_eq!(cpu_percent((0, start), (2_000_000, later)), 200.0);
    }

    #[test]
    fn a_cgroup_v1_payload_is_not_read_with_the_v2_layout() {
        // In v1, field 2 is the pids stat, whose `current` is field 1 too: read
        // as v2, a steady process count would pass for an idle CPU.
        let mut pids = Vec::new();
        encode_key(1, WireType::Varint, &mut pids);
        encode_varint(12, &mut pids);
        let mut top = Vec::new();
        encode_key(2, WireType::LengthDelimited, &mut top);
        encode_varint(pids.len() as u64, &mut top);
        top.extend_from_slice(&pids);

        assert_eq!(
            decode_metrics("io.containerd.cgroups.v1.Metrics", &top),
            CgroupSample::default()
        );
        assert_eq!(
            decode_metrics("io.containerd.cgroups.v2.Metrics", &top).cpu_usage_usec,
            Some(12)
        );
    }

    #[test]
    fn readings_are_measured_against_the_latest_stored_one() {
        let samples: CpuSamples = Default::default();
        let start = Instant::now();
        let second = start + Duration::from_secs(1);

        // First call: measured against its own first reading.
        let first = (0, start);
        assert_eq!(record(&samples, "a", Some(&first), (500_000, second)), 50.0);

        // Next call: against what the first one stored.
        let third = second + Duration::from_secs(1);
        assert_eq!(record(&samples, "a", None, (1_500_000, third)), 100.0);
        assert_eq!(samples.lock().unwrap()["a"], (1_500_000, third));
    }

    #[test]
    fn an_older_reading_never_replaces_a_newer_one() {
        let samples: CpuSamples = Default::default();
        let start = Instant::now();
        let newer = (2_000_000, start + Duration::from_secs(2));
        samples.lock().unwrap().insert("a".to_string(), newer);

        // A slower caller arrives with a reading taken before the stored one.
        let older = (1_000_000, start + Duration::from_secs(1));
        let percent = record(&samples, "a", None, older);

        assert_eq!(percent, 0.0, "no first reading to measure it against");
        assert_eq!(
            samples.lock().unwrap()["a"],
            newer,
            "the newer reading stays"
        );
    }

    #[test]
    fn cpu_percent_is_zero_on_a_reset_counter_or_no_elapsed_time() {
        let start = Instant::now();
        assert_eq!(
            cpu_percent((900, start), (100, start + Duration::from_secs(1))),
            0.0
        );
        assert_eq!(cpu_percent((0, start), (500, start)), 0.0);
    }
}
