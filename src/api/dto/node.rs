use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct NodeRootDto {
    pub hostname: String,
    pub os: String,
    pub arch: String,
    pub uptime: String,
    pub cpu_count: i64,
    pub memory_total: f64,   
    pub memory_available: f64,
    pub load_average: Vec<f64>,
}