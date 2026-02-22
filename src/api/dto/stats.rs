use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub(crate) struct ContainerStatsOutput {
    pub container_id: String,
    pub container_name: String,
    pub cpu_usage_percent: f64,
    pub memory: MemoryStats,
    pub network: NetworkStats,
    pub disk_io: DiskIoStats,
    pub pids: PidStats,
    pub restart_count: u64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub(crate) struct MemoryStats {
    pub usage_bytes: u64,
    pub limit_bytes: u64,
    pub usage_percent: f64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub(crate) struct NetworkStats {
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub rx_packets: u64,
    pub tx_packets: u64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub(crate) struct DiskIoStats {
    pub read_bytes: u64,
    pub write_bytes: u64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub(crate) struct PidStats {
    pub current: u64,
    pub limit: u64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub(crate) struct DeploymentStatsOutput {
    pub deployment_id: String,
    pub deployment_name: String,
    pub container_count: usize,
    pub total_cpu_usage_percent: f64,
    pub total_memory: MemoryStats,
    pub total_network: NetworkStats,
    pub total_disk_io: DiskIoStats,
    pub total_pids: u64,
    pub containers: Vec<ContainerStatsOutput>,
}
