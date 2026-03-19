use crate::models::namespace::Namespace;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub(crate) struct NamespaceOutput {
    pub id: String,
    pub created_at: String,
    pub updated_at: Option<String>,
    pub name: String,
}

impl NamespaceOutput {
    pub fn from_to_model(namespace: Namespace) -> Self {
        NamespaceOutput {
            id: namespace.id,
            created_at: namespace.created_at,
            updated_at: Option::from(namespace.updated_at.unwrap_or_default()),
            name: namespace.name,
        }
    }
}
