use serde::Deserialize;

#[derive(Deserialize, Debug, Clone)]
pub(crate) struct User {
    pub(crate) salt: String,
}
