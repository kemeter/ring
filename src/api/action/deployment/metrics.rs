use axum::extract::State;
use axum::http::StatusCode;
use axum::{Json, extract::Path, response::IntoResponse};

use crate::api::dto::stats::*;
use crate::api::server::{Db, RuntimeMap};
use crate::models::deployments;
use crate::models::users::User;

pub(crate) async fn metrics(
    Path(id): Path<String>,
    _user: User,
    State(pool): State<Db>,
    State(runtimes): State<RuntimeMap>,
) -> impl IntoResponse {
    match deployments::find(&pool, &id).await {
        Ok(Some(deployment)) => {
            let runtime = match runtimes.get(&deployment.runtime) {
                Some(rt) => rt,
                None => return StatusCode::NOT_FOUND.into_response(),
            };

            let instance_stats = runtime.get_instance_stats(&deployment.id).await;

            let instance_count = instance_stats.len();
            let total_cpu: f64 = instance_stats.iter().map(|c| c.cpu_usage_percent).sum();
            let total_mem_usage: u64 =
                instance_stats.iter().map(|c| c.memory.usage_bytes).sum();
            let total_mem_limit: u64 =
                instance_stats.iter().map(|c| c.memory.limit_bytes).sum();
            let total_rx: u64 = instance_stats.iter().map(|c| c.network.rx_bytes).sum();
            let total_tx: u64 = instance_stats.iter().map(|c| c.network.tx_bytes).sum();
            let total_rx_packets: u64 =
                instance_stats.iter().map(|c| c.network.rx_packets).sum();
            let total_tx_packets: u64 =
                instance_stats.iter().map(|c| c.network.tx_packets).sum();
            let total_read: u64 = instance_stats.iter().map(|c| c.disk_io.read_bytes).sum();
            let total_write: u64 =
                instance_stats.iter().map(|c| c.disk_io.write_bytes).sum();
            let total_pids: u64 = instance_stats.iter().map(|c| c.pids.current).sum();

            let output = DeploymentStatsOutput {
                deployment_id: deployment.id,
                deployment_name: deployment.name,
                instance_count,
                total_cpu_usage_percent: total_cpu,
                total_memory: MemoryStats {
                    usage_bytes: total_mem_usage,
                    limit_bytes: total_mem_limit,
                    usage_percent: if total_mem_limit > 0 {
                        (total_mem_usage as f64 / total_mem_limit as f64) * 100.0
                    } else {
                        0.0
                    },
                },
                total_network: NetworkStats {
                    rx_bytes: total_rx,
                    tx_bytes: total_tx,
                    rx_packets: total_rx_packets,
                    tx_packets: total_tx_packets,
                },
                total_disk_io: DiskIoStats {
                    read_bytes: total_read,
                    write_bytes: total_write,
                },
                total_pids,
                instances: instance_stats,
            };

            Json(output).into_response()
        }
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
