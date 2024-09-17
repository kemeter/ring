use serde::{Serialize, Deserialize};

#[derive(Deserialize, Serialize, Debug, Clone)]
pub(crate) struct UserOutput {
    pub(crate) id: String,
    pub(crate) username: String,
    pub(crate) created_at: String,
    pub(crate) updated_at: Option<String>,
    pub(crate) status: String,
    pub(crate) login_at: Option<String>
}
