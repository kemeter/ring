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

/// Emitted when the runtime fails to bring a deployment up (image pull,
/// container creation, network, config, resources, …). Carries the specific
/// `reason` and a `category` so a subscriber can triage user vs host vs
/// transient failures without parsing free text.
pub(crate) const KIND_DEPLOYMENT_ERROR: &str = "deployment.error";

/// Every event kind Ring can emit. Used to validate a webhook's subscription
/// filter at creation: a subscriber can't subscribe to a kind that will never
/// fire.
pub(crate) const KNOWN_EVENT_KINDS: &[&str] = &[
    KIND_DEPLOYMENT_STATUS_CHANGED,
    KIND_DEPLOYMENT_HEALTH_CHECK_FAILED,
    KIND_DEPLOYMENT_ROLLING_UPDATE,
    KIND_DEPLOYMENT_SCALED,
    KIND_DEPLOYMENT_ERROR,
];

/// Validate one entry of a webhook's `events` subscription filter, returning
/// `Err(reason)` with an actionable message when it is malformed.
///
/// Accepted forms (mirrors `Webhook::subscribes_to`):
/// - `*` — every kind,
/// - `<family>.*` — every kind in a family that actually exists (e.g.
///   `deployment.*`); a family with no known kind is rejected,
/// - an exact known kind.
///
/// Everything else is rejected loudly so a typo like `deployment*` (missing
/// dot) or `deployement.*` (misspelled family) fails at creation instead of
/// silently never matching.
pub(crate) fn validate_event_filter(entry: &str) -> Result<(), String> {
    if entry == "*" {
        return Ok(());
    }

    if let Some(prefix) = entry.strip_suffix(".*") {
        let known_families: Vec<&str> = KNOWN_EVENT_KINDS
            .iter()
            .filter_map(|k| k.split_once('.').map(|(family, _)| family))
            .collect();
        if known_families.contains(&prefix) {
            return Ok(());
        }
        return Err(format!(
            "unknown event family '{prefix}.*' (known families: {})",
            dedup_join(&known_families)
        ));
    }

    if KNOWN_EVENT_KINDS.contains(&entry) {
        return Ok(());
    }

    // Catch the common near-miss: a wildcard without the separating dot.
    if entry.ends_with('*') {
        return Err(format!(
            "invalid wildcard '{entry}' — use '*' for all kinds or '<family>.*' (e.g. 'deployment.*')"
        ));
    }

    Err(format!(
        "unknown event kind '{entry}' (known: {})",
        KNOWN_EVENT_KINDS.join(", ")
    ))
}

/// Join unique families in declaration order for an error message.
fn dedup_join(families: &[&str]) -> String {
    let mut seen: Vec<&str> = Vec::new();
    for f in families {
        if !seen.contains(f) {
            seen.push(f);
        }
    }
    seen.join(", ")
}

/// Classify a runtime error `reason` into a coarse `category` for triage, or
/// `None` if the reason is not a runtime error (so callers can tell which
/// internal events should surface as a `deployment.error` webhook).
///
/// - `user`: the operator's manifest is wrong — pulling the image, the config,
///   or the request can't succeed as written.
/// - `host`: the node can't satisfy the request right now (out of memory).
/// - `transient`: an infrastructure hiccup a retry may clear.
pub(crate) fn error_category(reason: &str) -> Option<&'static str> {
    match reason {
        "image_pull_back_off" | "config_error" => Some("user"),
        "insufficient_resources" => Some("host"),
        "instance_creation_failed"
        | "network_creation_failed"
        | "file_system_error"
        | "port_allocation_failed"
        | "vm_start_failed"
        | "firmware_not_found"
        | "stats_fetch_failed"
        | "runtime_error" => Some("transient"),
        _ => None,
    }
}

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

    /// Build a `deployment.error` event. `reason` is the runtime error
    /// discriminant (e.g. `image_pull_back_off`), `category` its triage class
    /// (see [`error_category`]), and `message` the operator-facing detail.
    pub(crate) fn deployment_error(
        deployment: &Deployment,
        reason: &str,
        category: &str,
        message: &str,
    ) -> Self {
        Event {
            kind: KIND_DEPLOYMENT_ERROR.to_string(),
            payload: json!({
                "schema_version": SCHEMA_VERSION,
                "deployment_id": deployment.id,
                "namespace": deployment.namespace,
                "name": deployment.name,
                "kind": deployment.kind,
                "reason": reason,
                "category": category,
                "message": message,
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
            error!("Failed to serialize event {} payload: {}", event.kind, e);
            return;
        }
    };
    if let Err(e) = event_queue::enqueue(pool, &event.kind, &payload).await {
        warn!("Failed to enqueue event {}: {}", event.kind, e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_category_classifies_runtime_reasons() {
        assert_eq!(error_category("image_pull_back_off"), Some("user"));
        assert_eq!(error_category("config_error"), Some("user"));
        assert_eq!(error_category("insufficient_resources"), Some("host"));
        assert_eq!(
            error_category("instance_creation_failed"),
            Some("transient")
        );
        assert_eq!(error_category("network_creation_failed"), Some("transient"));
    }

    #[test]
    fn error_category_ignores_non_runtime_reasons() {
        // Health-check alerts are error-level but not a deployment.error.
        assert_eq!(error_category("health_check_alert"), None);
        assert_eq!(error_category("scale_up"), None);
        assert_eq!(error_category("state_transition"), None);
    }

    #[test]
    fn validate_event_filter_accepts_wildcards_and_known_kinds() {
        assert!(validate_event_filter("*").is_ok());
        assert!(validate_event_filter("deployment.*").is_ok());
        assert!(validate_event_filter("deployment.scaled").is_ok());
    }

    #[test]
    fn validate_event_filter_rejects_unknown_family() {
        // A misspelled family must fail loudly, not silently never match.
        let err = validate_event_filter("deployement.*").unwrap_err();
        assert!(err.contains("unknown event family"), "{err}");
        assert!(err.contains("deployement.*"), "{err}");
    }

    #[test]
    fn validate_event_filter_rejects_wildcard_without_dot() {
        // The classic typo: `deployment*` instead of `deployment.*`.
        let err = validate_event_filter("deployment*").unwrap_err();
        assert!(err.contains("invalid wildcard"), "{err}");
        assert!(err.contains("<family>.*"), "{err}");
    }

    #[test]
    fn validate_event_filter_rejects_unknown_exact_kind() {
        let err = validate_event_filter("bogus.kind").unwrap_err();
        assert!(err.contains("unknown event kind"), "{err}");
    }
}
