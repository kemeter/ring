use crate::models::namespace::Namespace;
use crate::models::security_level::SecurityLevel;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub(crate) struct NamespaceOutput {
    pub id: String,
    pub created_at: String,
    pub updated_at: Option<String>,
    pub name: String,
    /// The level its deployments are held to: its own, else the server's
    /// default.
    pub security_level: SecurityLevel,
}

impl NamespaceOutput {
    pub fn from_to_model(namespace: Namespace, default_level: SecurityLevel) -> Self {
        NamespaceOutput {
            id: namespace.id,
            created_at: namespace.created_at,
            updated_at: Option::from(namespace.updated_at.unwrap_or_default()),
            name: namespace.name,
            security_level: namespace.security_level.unwrap_or(default_level),
        }
    }
}
