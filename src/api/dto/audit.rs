use crate::models::audit_log::AuditEntry;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub(crate) struct AuditOutput {
    pub id: String,
    pub timestamp: String,
    pub user_id: Option<String>,
    pub action: String,
    pub target_type: String,
    pub target_name: String,
    pub namespace: Option<String>,
}

impl AuditOutput {
    pub fn from_to_model(entry: AuditEntry) -> Self {
        AuditOutput {
            id: entry.id,
            timestamp: entry.timestamp,
            user_id: entry.user_id,
            action: entry.action,
            target_type: entry.target_type,
            target_name: entry.target_name,
            namespace: entry.namespace,
        }
    }
}
