use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub(crate) struct Api {
    pub(crate) port: u16,
    pub(crate) scheme: String,
    #[serde(default)]
    pub(crate) cors_origins: Vec<String>,
}
