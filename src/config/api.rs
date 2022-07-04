use serde::Deserialize;

#[derive(Deserialize, Debug, Clone)]
pub(crate) struct Api {
    pub(crate) port: u16,
    pub(crate) scheme: String
}
