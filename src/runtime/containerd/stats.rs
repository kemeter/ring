//! Per-instance resource statistics via the Tasks `Metrics` RPC.
//!
//! `Tasks.Metrics` returns a `containerd.types.Metric` whose `data` is a
//! protobuf `Any` carrying the cgroup metrics — for cgroup v2 hosts the type is
//! `io.containerd.cgroups.v2.Metrics`, for v1 `io.containerd.cgroups.v1.Metrics`.
//! Those message definitions live in the containerd `cgroups` crate, which this
//! build does **not** depend on, so we cannot strongly-decode them here.
//!
//! Rather than pull in another proto crate, we decode the two fields Ring's
//! dashboard cares about — memory usage and pids — directly from the cgroup v2
//! `Metrics` wire format using prost's field-level reader. CPU percentage and
//! network counters are not derivable from a single cgroup sample without a
//! prior reading and an interface accounting source, so they are reported as
//! zero (documented limitation; matches what cgroup v2 exposes for a container
//! that has no per-interface accounting).

use crate::api::dto::stats::*;
use containerd_client::services::v1::MetricsRequest;
use containerd_client::services::v1::tasks_client::TasksClient;
use containerd_client::with_namespace;
use prost::bytes::Buf;
use prost::encoding::{DecodeContext, WireType, decode_key, skip_field};
use tonic::Request;

/// Fetch and map one instance's stats. Returns `None` when the task has no
/// metrics (not running) so the caller simply omits it.
pub(crate) async fn fetch_instance_stats(
    client: &containerd_client::Client,
    namespace: &str,
    instance_id: &str,
    instance_name: &str,
) -> Option<InstanceStatsOutput> {
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

    let (mem_usage, mem_limit, pids_current, pids_limit) = decode_cgroup_v2(&data.value);

    Some(InstanceStatsOutput {
        instance_id: instance_id.chars().take(12).collect(),
        instance_name: instance_name.to_string(),
        // CPU percentage needs a delta between two samples + system time; a
        // single Metrics call cannot produce it. Reported as 0 until a
        // sampling loop is added.
        cpu_usage_percent: 0.0,
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
            current: pids_current,
            limit: pids_limit,
        },
        // containerd tracks restarts via the shim, not the metrics RPC; Ring's
        // own restart_count on the deployment is authoritative, so 0 here.
        restart_count: 0,
    })
}

/// Best-effort field-level decode of an `io.containerd.cgroups.v2.Metrics`
/// message, extracting `(memory.usage, memory.usage_limit, pids.current,
/// pids.limit)`.
///
/// The v2 `Metrics` layout (from containerd's `cgroups/v2/stats.proto`):
///   field 4  = Pids   { current=1 (uint64), limit=2 (uint64) }
///   field 5  = Memory { ... usage=10 (uint64), usage_limit=11 (uint64) ... }
///
/// We walk the top-level message, recurse into the Pids and Memory submessages,
/// and read the specific tags. Unknown fields are skipped. Returns zeros for any
/// field absent (e.g. on a cgroup v1 host, where the layout differs and nothing
/// matches — the values stay 0, which the caller renders as "unknown").
fn decode_cgroup_v2(mut buf: &[u8]) -> (u64, u64, u64, u64) {
    let mut mem_usage = 0u64;
    let mut mem_limit = 0u64;
    let mut pids_current = 0u64;
    let mut pids_limit = 0u64;

    while buf.has_remaining() {
        let Ok((tag, wire)) = decode_key(&mut buf) else {
            break;
        };
        match (tag, wire) {
            // Pids submessage (Metrics.pids, field 1).
            (1, WireType::LengthDelimited) => {
                if let Some(sub) = read_len_delimited(&mut buf) {
                    let (c, l) = decode_pids(sub);
                    pids_current = c;
                    pids_limit = l;
                }
            }
            // Memory submessage (Metrics.memory, field 4).
            (4, WireType::LengthDelimited) => {
                if let Some(sub) = read_len_delimited(&mut buf) {
                    let (u, l) = decode_memory(sub);
                    mem_usage = u;
                    mem_limit = l;
                }
            }
            _ => {
                if skip_field(wire, tag, &mut buf, DecodeContext::default()).is_err() {
                    break;
                }
            }
        }
    }
    (mem_usage, mem_limit, pids_current, pids_limit)
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

        let (mu, ml, pc, pl) = decode_cgroup_v2(&top);
        assert_eq!((mu, ml, pc, pl), (2048, 4096, 3, 50));
    }

    #[test]
    fn decode_empty_is_zeros() {
        assert_eq!(decode_cgroup_v2(&[]), (0, 0, 0, 0));
    }
}
