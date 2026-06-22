//! `/metrics` endpoint with two output formats negotiated via `Accept`.
//!
//! Reads the live inventory from the database and renders it in either:
//!
//! - Prometheus text exposition `version=0.0.4` (default — what Prometheus
//!   scrapers expect when no `Accept` is sent);
//! - JSON, when the client sends `Accept: application/json`. Same data,
//!   structured for direct consumption by a dashboard or any other client that
//!   prefers to skip the text-parsing step.
//!
//! No moving parts, no background aggregator — every scrape recomputes from the
//! database, so values are always consistent with what the rest of the API
//! would return.
//!
//! Endpoint is intentionally unauthenticated: Prometheus scrapers default to no
//! auth and operators front the API with TLS / network ACLs anyway.
//!
//! This is the lightweight inventory snapshot (counts only). Per-deployment
//! resource usage (CPU/memory/network) lives behind `/deployments/{id}/metrics`
//! because querying every runtime on each scrape would be expensive; it can be
//! folded in here later as an additive section without breaking this format.

use crate::api::server::AppState;
use crate::models::deployments::DeploymentStatus;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use std::collections::BTreeMap;
use std::fmt::Write;
use tracing::error;

const PROM_CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";
const JSON_CONTENT_TYPE: &str = "application/json; charset=utf-8";

pub(crate) async fn metrics(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let snap = Snapshot::collect(&state.connection).await;

    let (body, content_type) = if wants_json(&headers) {
        let body = serde_json::to_string(&snap).unwrap_or_else(|e| {
            error!("metrics: JSON serialization failed: {}", e);
            "{}".to_string()
        });
        (body, JSON_CONTENT_TYPE)
    } else {
        (render_prom(&snap), PROM_CONTENT_TYPE)
    };

    let mut response = (StatusCode::OK, body).into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    response
}

/// Returns true when the request's `Accept` header asks for JSON. Anything
/// else — including no `Accept`, `*/*`, or the Prometheus content-type — falls
/// back to Prometheus text so existing scrapers are never surprised by a JSON
/// body.
fn wants_json(headers: &HeaderMap) -> bool {
    let Some(accept) = headers.get(header::ACCEPT).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    accept
        .split(',')
        .map(|part| part.split(';').next().unwrap_or("").trim())
        .any(|media| media.eq_ignore_ascii_case("application/json"))
}

/// Numeric snapshot of every metric we expose, computed once per scrape from
/// the database. Both renderers (Prometheus text and JSON) read from this
/// struct so the two formats can never drift on values.
///
/// The `deployments_by_*` maps use `BTreeMap` so label order is deterministic
/// across scrapes.
#[derive(Debug, Default, Serialize)]
struct Snapshot {
    /// Deployments excluding the `deleted` tombstone status — the count an
    /// operator means by "how many deployments do I have".
    deployments: u64,
    deployments_by_status: BTreeMap<String, u64>,
    deployments_by_runtime: BTreeMap<String, u64>,
    namespaces: u64,
    secrets: u64,
    volumes: u64,
    users: u64,
    webhooks: u64,
    configs: u64,
    /// Outbound event queue by delivery status. `pending` is the live queue
    /// depth, `dead` is the dead-letter count — both are alerting signals.
    events_by_status: BTreeMap<String, u64>,
    /// Health-check results by status; failing checks surface here.
    health_checks_by_status: BTreeMap<String, u64>,
}

/// Delivery statuses of the outbound event queue, always emitted so the series
/// never vanish between scrapes. Mirrors the `events.status` domain documented
/// in migration `20220101000019`.
const EVENT_STATUSES: [&str; 3] = ["pending", "delivered", "dead"];

impl Snapshot {
    async fn collect(pool: &sqlx::SqlitePool) -> Self {
        let mut snap = Snapshot::default();

        // Seed every known deployment status at 0, then overlay the DB counts.
        // A status with no rows still gets a series so alerts never break.
        let status_keys = DeploymentStatus::all().map(|s| s.to_string());
        snap.deployments_by_status = with_zero_keys(
            group_count(pool, "deployment", "status").await,
            status_keys.iter().map(String::as_str),
        );
        snap.deployments_by_runtime = group_count(pool, "deployment", "runtime").await;
        snap.deployments = snap
            .deployments_by_status
            .iter()
            .filter(|(status, _)| status.as_str() != "deleted")
            .map(|(_, count)| count)
            .sum();

        snap.events_by_status = with_zero_keys(
            group_count(pool, "events", "status").await,
            EVENT_STATUSES.iter().copied(),
        );
        snap.health_checks_by_status = group_count(pool, "health_check", "status").await;

        snap.namespaces = table_count(pool, "namespace").await;
        snap.secrets = table_count(pool, "secret").await;
        snap.volumes = table_count(pool, "volumes").await;
        snap.users = table_count(pool, "user").await;
        snap.webhooks = table_count(pool, "webhook").await;
        snap.configs = table_count(pool, "config").await;

        snap
    }
}

/// `SELECT COUNT(*)` for a table. A query error logs and yields `0` rather than
/// failing the whole scrape — a single broken series should never take the
/// endpoint down for a scraper.
async fn table_count(pool: &sqlx::SqlitePool, table: &str) -> u64 {
    // `table` is a hard-coded literal at every call site, never user input, so
    // the format!-built query carries no injection surface.
    let sql = format!("SELECT COUNT(*) FROM {table}");
    match sqlx::query_scalar::<_, i64>(&sql).fetch_one(pool).await {
        Ok(count) => count.max(0) as u64,
        Err(e) => {
            error!("metrics: counting {} failed: {}", table, e);
            0
        }
    }
}

/// `SELECT col, COUNT(*) ... GROUP BY col` as a label→count map. Same
/// fail-soft contract as [`table_count`].
async fn group_count(pool: &sqlx::SqlitePool, table: &str, column: &str) -> BTreeMap<String, u64> {
    // `table`/`column` are hard-coded literals at every call site.
    let sql = format!("SELECT {column}, COUNT(*) FROM {table} GROUP BY {column}");
    match sqlx::query_as::<_, (String, i64)>(&sql)
        .fetch_all(pool)
        .await
    {
        Ok(rows) => rows
            .into_iter()
            .map(|(label, count)| (label, count.max(0) as u64))
            .collect(),
        Err(e) => {
            error!("metrics: grouping {}.{} failed: {}", table, column, e);
            BTreeMap::new()
        }
    }
}

/// Ensure every expected label value is present, defaulting to 0, without
/// dropping any extra keys the DB returned. Keeps zero-valued series alive
/// across scrapes (Prometheus alerts break when a series disappears) while
/// still surfacing unforeseen values.
fn with_zero_keys<'a>(
    mut counts: BTreeMap<String, u64>,
    expected: impl Iterator<Item = &'a str>,
) -> BTreeMap<String, u64> {
    for key in expected {
        counts.entry(key.to_string()).or_insert(0);
    }
    counts
}

fn render_prom(snap: &Snapshot) -> String {
    let mut out = String::with_capacity(1024);

    write_gauge(
        &mut out,
        "ring_deployments",
        "Number of deployments, excluding deleted ones.",
        snap.deployments,
    );

    write_labelled_gauge(
        &mut out,
        "ring_deployments_by_status",
        "Number of deployments per status (includes deleted).",
        "status",
        &snap.deployments_by_status,
    );
    write_labelled_gauge(
        &mut out,
        "ring_deployments_by_runtime",
        "Number of deployments per runtime (includes deleted).",
        "runtime",
        &snap.deployments_by_runtime,
    );

    write_gauge(
        &mut out,
        "ring_namespaces",
        "Number of namespaces.",
        snap.namespaces,
    );
    write_gauge(&mut out, "ring_secrets", "Number of secrets.", snap.secrets);
    write_gauge(&mut out, "ring_volumes", "Number of volumes.", snap.volumes);
    write_gauge(&mut out, "ring_users", "Number of users.", snap.users);
    write_gauge(
        &mut out,
        "ring_webhooks",
        "Number of webhooks.",
        snap.webhooks,
    );
    write_gauge(&mut out, "ring_configs", "Number of configs.", snap.configs);

    write_labelled_gauge(
        &mut out,
        "ring_events_by_status",
        "Outbound events per delivery status (pending = queue depth, dead = dead-lettered).",
        "status",
        &snap.events_by_status,
    );
    write_labelled_gauge(
        &mut out,
        "ring_health_checks_by_status",
        "Health-check results per status.",
        "status",
        &snap.health_checks_by_status,
    );

    out
}

fn write_gauge(out: &mut String, name: &str, help: &str, value: u64) {
    let _ = writeln!(out, "# HELP {name} {help}");
    let _ = writeln!(out, "# TYPE {name} gauge");
    let _ = writeln!(out, "{name} {value}");
}

/// Render a gauge with one series per label value. Emits the HELP/TYPE pair
/// once, then a line per entry. Label values are escaped per the Prometheus
/// exposition rules (`\`, `"`, newline).
fn write_labelled_gauge(
    out: &mut String,
    name: &str,
    help: &str,
    label: &str,
    values: &BTreeMap<String, u64>,
) {
    let _ = writeln!(out, "# HELP {name} {help}");
    let _ = writeln!(out, "# TYPE {name} gauge");
    for (key, count) in values {
        let _ = writeln!(
            out,
            "{name}{{{label}=\"{}\"}} {count}",
            escape_label_value(key)
        );
    }
}

/// Escape a Prometheus label value: backslash, double-quote and newline, per
/// the text exposition format.
fn escape_label_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::server::tests::{login, new_test_app, new_test_app_with_pool};
    use axum_test::TestServer;
    use http::StatusCode;
    use serde_json::json;

    #[test]
    fn escape_label_value_escapes_special_chars() {
        assert_eq!(escape_label_value(r#"a"b\c"#), r#"a\"b\\c"#);
        assert_eq!(escape_label_value("plain"), "plain");
    }

    #[test]
    fn wants_json_recognizes_application_json() {
        let mut h = HeaderMap::new();
        h.insert(header::ACCEPT, "application/json".parse().unwrap());
        assert!(wants_json(&h));
    }

    #[test]
    fn wants_json_handles_multiple_media_types() {
        let mut h = HeaderMap::new();
        h.insert(
            header::ACCEPT,
            "text/html, application/json;q=0.9, */*;q=0.8"
                .parse()
                .unwrap(),
        );
        assert!(wants_json(&h));
    }

    #[test]
    fn wants_json_defaults_to_false_when_absent() {
        assert!(!wants_json(&HeaderMap::new()));
    }

    #[tokio::test]
    async fn metrics_is_public_and_prometheus_text() {
        let server = TestServer::new(new_test_app().await).unwrap();
        let response = server.get("/metrics").await;

        assert_eq!(response.status_code(), StatusCode::OK);
        assert_eq!(
            response.header(header::CONTENT_TYPE).to_str().unwrap(),
            "text/plain; version=0.0.4; charset=utf-8"
        );

        let body = response.text();
        assert!(body.contains("# HELP ring_deployments"));
        assert!(body.contains("# TYPE ring_deployments gauge"));
        assert!(body.contains("ring_namespaces"));
        // The seeded fixtures include the admin user.
        assert!(body.contains("ring_users"));
    }

    #[tokio::test]
    async fn metrics_accept_json_returns_json() {
        let server = TestServer::new(new_test_app().await).unwrap();
        let response = server
            .get("/metrics")
            .add_header(header::ACCEPT, "application/json")
            .await;

        assert_eq!(response.status_code(), StatusCode::OK);
        assert_eq!(
            response.header(header::CONTENT_TYPE).to_str().unwrap(),
            "application/json; charset=utf-8"
        );

        let json: serde_json::Value = response.json();
        assert!(json["deployments"].is_number());
        assert!(json["deployments_by_status"].is_object());
        assert!(json["users"].is_number());
    }

    #[test]
    fn with_zero_keys_seeds_missing_and_keeps_extra() {
        let mut counts = BTreeMap::new();
        counts.insert("pending".to_string(), 3);
        counts.insert("unexpected".to_string(), 1);
        let out = with_zero_keys(counts, EVENT_STATUSES.iter().copied());
        assert_eq!(out.get("pending"), Some(&3));
        assert_eq!(out.get("delivered"), Some(&0));
        assert_eq!(out.get("dead"), Some(&0));
        // An unforeseen DB value is preserved, not dropped.
        assert_eq!(out.get("unexpected"), Some(&1));
    }

    #[tokio::test]
    async fn metrics_emit_all_statuses_even_at_zero() {
        let server = TestServer::new(new_test_app().await).unwrap();
        let body = server.get("/metrics").await.text();

        // Error statuses that have no rows in fixtures must still be present.
        assert!(body.contains("ring_deployments_by_status{status=\"crash_loop_back_off\"}"));
        assert!(body.contains("ring_deployments_by_status{status=\"image_pull_back_off\"}"));
        // Event queue statuses are always emitted.
        assert!(body.contains("ring_events_by_status{status=\"pending\"}"));
        assert!(body.contains("ring_events_by_status{status=\"dead\"}"));
    }

    #[tokio::test]
    async fn metrics_counts_deployments_and_excludes_deleted_from_total() {
        let (pool, app) = new_test_app_with_pool().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        // The fixtures already seed some deployments; measure the delta we add
        // rather than an absolute count.
        let before = Snapshot::collect(&pool).await;
        let docker_before = before
            .deployments_by_runtime
            .get("docker")
            .copied()
            .unwrap_or(0);

        for name in ["one", "two"] {
            let create = server
                .post("/deployments")
                .add_header("Authorization", format!("Bearer {}", token))
                .json(&json!({
                    "runtime": "docker",
                    "name": name,
                    "namespace": "test",
                    "image": "nginx:latest",
                }))
                .await;
            assert_eq!(create.status_code(), StatusCode::CREATED);
        }

        let after = Snapshot::collect(&pool).await;
        assert_eq!(after.deployments, before.deployments + 2);
        assert_eq!(
            after.deployments_by_runtime.get("docker").copied(),
            Some(docker_before + 2)
        );
    }
}
