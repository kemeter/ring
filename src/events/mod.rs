//! Ring's outbound event bus.
//!
//! Any part of Ring publishes a typed [`Event`] via [`publish`]; it is durably
//! enqueued and later delivered by the event worker to every webhook subscribed
//! to its `kind`. Producers know nothing about webhooks or delivery — they just
//! describe *what happened*.
//!
//! The bus is generic: `deployment.status_changed` is merely the first `kind`.
//! New event types are added by introducing a `kind` here and calling `publish`
//! from wherever the event originates.

use crate::models::deployments::{Deployment, DeploymentStatus};
use crate::models::event_queue;
use serde::Serialize;
use serde_json::json;
use sqlx::SqlitePool;

/// Bumped on any breaking change to a payload contract. Receivers can branch on
/// it.
pub(crate) const SCHEMA_VERSION: u32 = 1;

/// Emitted on every deployment status transition.
pub(crate) const KIND_DEPLOYMENT_STATUS_CHANGED: &str = "deployment.status_changed";

/// Every event kind Ring can emit. Used to validate a webhook's subscription
/// filter at creation: a subscriber can't subscribe to a kind that will never
/// fire.
pub(crate) const KNOWN_EVENT_KINDS: &[&str] = &[KIND_DEPLOYMENT_STATUS_CHANGED];

/// A typed event ready to publish. `payload` is the JSON body delivered
/// verbatim to subscribers, wrapped by the worker in the signed envelope.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Event {
    pub(crate) kind: String,
    pub(crate) payload: serde_json::Value,
}

impl Event {
    /// Build a `deployment.status_changed` event from a deployment carrying its
    /// new status and the status it transitioned from.
    pub(crate) fn deployment_status_changed(
        deployment: &Deployment,
        old_status: &DeploymentStatus,
    ) -> Self {
        Event {
            kind: KIND_DEPLOYMENT_STATUS_CHANGED.to_string(),
            payload: json!({
                "schema_version": SCHEMA_VERSION,
                "deployment_id": deployment.id,
                "namespace": deployment.namespace,
                "name": deployment.name,
                "kind": deployment.kind,
                "old_status": old_status.to_string(),
                "new_status": deployment.status.to_string(),
                "restart_count": deployment.restart_count,
            }),
        }
    }
}

/// Publish an event: durably enqueue it for delivery. The single entry point
/// producers use. Best-effort at the call site — a failed enqueue is logged and
/// swallowed so it never breaks the producer's own work (e.g. a scheduler tick).
pub(crate) async fn publish(pool: &SqlitePool, event: Event) {
    let payload = match serde_json::to_string(&event.payload) {
        Ok(p) => p,
        Err(e) => {
            log::error!("Failed to serialize event {} payload: {}", event.kind, e);
            return;
        }
    };
    if let Err(e) = event_queue::enqueue(pool, &event.kind, &payload).await {
        log::warn!("Failed to enqueue event {}: {}", event.kind, e);
    }
}
