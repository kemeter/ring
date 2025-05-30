use serde::{Deserialize, Serialize};
use crate::models::config::{Config};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub(crate) struct ConfigOutput {
    pub id: String,
    pub created_at: String,
    pub updated_at: Option<String>,
    pub namespace: String,
    pub name: String,
    pub data: String,
    pub labels: String,
}

impl ConfigOutput {
    pub fn from_to_model(config: Config) -> Self {
        ConfigOutput {
            id: config.id.to_string(),
            created_at: config.created_at.to_string(),
            updated_at: Option::from(config.updated_at.unwrap_or_default()),
            namespace: config.namespace,
            name: config.name,
            data: config.data,
            labels: config.labels,
        }
    }
}