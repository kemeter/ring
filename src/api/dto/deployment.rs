use serde::{Serialize, Deserialize};
use std::collections::HashMap;

#[derive(Deserialize, Serialize, Debug, Clone)]
pub(crate) struct DeploymentDTO {
    pub(crate) id: String,
    pub(crate) created_at: String,
    pub(crate) status: String,
    pub(crate) name: String,
    pub(crate) runtime: String,
    pub(crate) namespace: String,
    pub(crate) image: String,
    pub(crate) replicas: i64,
    pub(crate) ports: Vec<String>,
    pub(crate) labels: HashMap<String, String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub(crate) instances: Vec<String>
}