pub(crate) mod create;
pub(crate) mod delete;
pub(crate) mod events;
pub(crate) mod list;

pub(crate) use create::create;
pub(crate) use delete::delete;
pub(crate) use events::events;
pub(crate) use list::list;

use crate::models::webhook::Webhook;
use serde::Serialize;

/// Secret-free projection of a webhook for list responses.
#[derive(Serialize)]
pub(crate) struct WebhookView {
    pub(crate) id: String,
    pub(crate) url: String,
    pub(crate) events: Vec<String>,
    pub(crate) created_at: String,
    pub(crate) revoked_at: Option<String>,
}

impl From<Webhook> for WebhookView {
    fn from(w: Webhook) -> Self {
        WebhookView {
            id: w.id,
            url: w.url,
            events: w.events,
            created_at: w.created_at,
            revoked_at: w.revoked_at,
        }
    }
}
