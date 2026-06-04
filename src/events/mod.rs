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

/// Emitted when a health check fails enough to trigger its `on_failure` action
/// (restart / stop / alert) — an early warning a service is unhealthy, before
/// it cascades into a status change.
pub(crate) const KIND_DEPLOYMENT_HEALTH_CHECK_FAILED: &str = "deployment.health_check_failed";

/// Emitted as a rolling update progresses: a parent instance drained (`step`),
/// the rollout finished (`complete`), or it was abandoned because the child
/// never became healthy (`failed`).
pub(crate) const KIND_DEPLOYMENT_ROLLING_UPDATE: &str = "deployment.rolling_update";

/// Emitted when the reconciler adds or removes one instance to converge on the
/// deployment's `replicas`.
pub(crate) const KIND_DEPLOYMENT_SCALED: &str = "deployment.scaled";

/// Every event kind Ring can emit. Used to validate a webhook's subscription
/// filter at creation: a subscriber can't subscribe to a kind that will never
/// fire.
pub(crate) const KNOWN_EVENT_KINDS: &[&str] = &[
    KIND_DEPLOYMENT_STATUS_CHANGED,
    KIND_DEPLOYMENT_HEALTH_CHECK_FAILED,
    KIND_DEPLOYMENT_ROLLING_UPDATE,
    KIND_DEPLOYMENT_SCALED,
];

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

    /// Build a `deployment.health_check_failed` event. `action` is the
    /// `on_failure` action that fired (`restart` / `stop` / `alert`),
    /// `instance_id` the instance that failed its probe, and `message` the
    /// probe's failure detail.
    pub(crate) fn deployment_health_check_failed(
        deployment: &Deployment,
        instance_id: &str,
        action: &str,
        message: &str,
    ) -> Self {
        Event {
            kind: KIND_DEPLOYMENT_HEALTH_CHECK_FAILED.to_string(),
            payload: json!({
                "schema_version": SCHEMA_VERSION,
                "deployment_id": deployment.id,
                "namespace": deployment.namespace,
                "name": deployment.name,
                "kind": deployment.kind,
                "instance_id": instance_id,
                "action": action,
                "message": message,
            }),
        }
    }

    /// Build a `deployment.rolling_update` event for the *child* deployment
    /// being rolled out. `phase` is `step` (a parent instance was drained),
    /// `complete` (parent fully replaced), or `failed` (child never became
    /// healthy; parent left running). `drained_instance_id` is set on `step`.
    pub(crate) fn deployment_rolling_update(
        child: &Deployment,
        parent_id: &str,
        phase: &str,
        drained_instance_id: Option<&str>,
    ) -> Self {
        Event {
            kind: KIND_DEPLOYMENT_ROLLING_UPDATE.to_string(),
            payload: json!({
                "schema_version": SCHEMA_VERSION,
                "deployment_id": child.id,
                "namespace": child.namespace,
                "name": child.name,
                "kind": child.kind,
                "parent_id": parent_id,
                "phase": phase,
                "drained_instance_id": drained_instance_id,
            }),
        }
    }

    /// Build a `deployment.scaled` event. `direction` is `up` or `down`;
    /// `instance_count` is the live instance count after the change, `replicas`
    /// the desired target the reconciler is converging on.
    pub(crate) fn deployment_scaled(
        deployment: &Deployment,
        direction: &str,
        instance_count: usize,
    ) -> Self {
        Event {
            kind: KIND_DEPLOYMENT_SCALED.to_string(),
            payload: json!({
                "schema_version": SCHEMA_VERSION,
                "deployment_id": deployment.id,
                "namespace": deployment.namespace,
                "name": deployment.name,
                "kind": deployment.kind,
                "direction": direction,
                "instance_count": instance_count,
                "replicas": deployment.replicas,
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
