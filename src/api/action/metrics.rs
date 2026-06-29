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
//! The inventory series (counts by status/runtime, table totals) are recomputed
//! from the database on every scrape, so they are always consistent with what
//! the rest of the API would return.
//!
//! Endpoint is intentionally unauthenticated: Prometheus scrapers default to no
//! auth and operators front the API with TLS / network ACLs anyway.
//!
//! Per-deployment resource usage (CPU/memory/network) is NOT read on the scrape
//! path — querying every runtime on each scrape would be expensive. It comes
//! from a background-refreshed snapshot (`scheduler::stats_cache`), so those
//! series are at most one refresh interval stale; `ring_runtime_last_refresh_seconds`
//! exposes that staleness. For a fresh point-in-time read of one deployment,
//! use `/deployments/{id}/metrics`.

use crate::api::server::AppState;
use crate::models::deployments;
use crate::models::deployments::DeploymentStatus;
use crate::models::query::{group_count, table_count};
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
    let mut snap = Snapshot::collect(&state.connection).await;

    // Runtime resource usage comes from the background cache, never the request
    // path, so the scrape stays cheap. A poisoned lock yields no runtime series
    // (logged) rather than failing the whole scrape.
    match state.stats_cache.read() {
        Ok(guard) => {
            snap.runtime_last_refresh_seconds = guard.last_refresh_unix;
            snap.deployment_runtime = guard.deployments.iter().map(RuntimeSeries::from).collect();
        }
        Err(e) => error!("metrics: stats cache lock poisoned: {}", e),
    }

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
    /// Deployments stuck in a non-healthy state, broken down by
    /// `(namespace, status)`. Only unhealthy statuses are emitted (running /
    /// completed / deleted are excluded), so the series count tracks the number
    /// of deployments actually in trouble rather than namespaces × statuses.
    /// Lets an operator see *which* tenant is affected, not just the total.
    unhealthy_deployments_by_namespace: Vec<NamespaceStatusCount>,
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
    /// Unix timestamp of the last successful runtime-stats refresh; `0` if the
    /// background worker has not completed a cycle yet.
    runtime_last_refresh_seconds: u64,
    /// Per-deployment runtime resource usage, sourced from the background
    /// cache (not the DB). Empty until the first refresh, or when no active
    /// deployment reports stats.
    deployment_runtime: Vec<RuntimeSeries>,
}

/// One `(namespace, status)` bucket with its deployment count, ready to render
/// as a two-label Prometheus series.
#[derive(Debug, Serialize)]
struct NamespaceStatusCount {
    namespace: String,
    status: String,
    count: u64,
}

/// Statuses considered "in trouble": a deployment in one of these is not
/// serving and needs attention. `running`/`completed`/`deleted` are healthy or
/// terminal and intentionally excluded so the per-namespace breakdown stays
/// small and alert-friendly. Listed explicitly (not "everything else") so a new
/// status is a deliberate decision, not silently bucketed.
const UNHEALTHY_STATUSES: [&str; 10] = [
    "pending",
    "failed",
    "crash_loop_back_off",
    "image_pull_back_off",
    "create_container_error",
    "network_error",
    "config_error",
    "file_system_error",
    "insufficient_resources",
    "error",
];

/// Serializable, render-ready copy of one deployment's cached runtime stats.
/// Mirrors `stats_cache::DeploymentRuntimeStats` but lives here so the cache
/// type need not depend on serde or the metrics format.
#[derive(Debug, Serialize)]
struct RuntimeSeries {
    name: String,
    namespace: String,
    runtime: String,
    instances: u64,
    cpu_usage_percent: f64,
    memory_usage_bytes: u64,
    memory_limit_bytes: u64,
    network_rx_bytes: u64,
    network_tx_bytes: u64,
    disk_read_bytes: u64,
    disk_write_bytes: u64,
    pids: u64,
    restarts: u64,
}

impl From<&crate::scheduler::stats_cache::DeploymentRuntimeStats> for RuntimeSeries {
    fn from(s: &crate::scheduler::stats_cache::DeploymentRuntimeStats) -> Self {
        RuntimeSeries {
            name: s.name.clone(),
            namespace: s.namespace.clone(),
            runtime: s.runtime.clone(),
            instances: s.instance_count,
            cpu_usage_percent: s.cpu_usage_percent,
            memory_usage_bytes: s.memory_usage_bytes,
            memory_limit_bytes: s.memory_limit_bytes,
            network_rx_bytes: s.network_rx_bytes,
            network_tx_bytes: s.network_tx_bytes,
            disk_read_bytes: s.disk_read_bytes,
            disk_write_bytes: s.disk_write_bytes,
            pids: s.pids,
            restarts: s.restarts,
        }
    }
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
        snap.unhealthy_deployments_by_namespace =
            unhealthy_by_namespace(pool, &UNHEALTHY_STATUSES).await;
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

/// Render-ready breakdown of deployments in trouble, per `(namespace, status)`.
/// Thin adapter over [`deployments::count_by_namespace_and_status`]: maps the
/// raw rows into [`NamespaceStatusCount`] and, like the other metrics helpers,
/// is fail-soft — a query error logs and yields an empty list rather than
/// taking the whole scrape down.
async fn unhealthy_by_namespace(
    pool: &sqlx::SqlitePool,
    statuses: &[&str],
) -> Vec<NamespaceStatusCount> {
    match deployments::count_by_namespace_and_status(pool, statuses).await {
        Ok(rows) => rows
            .into_iter()
            .map(|(namespace, status, count)| NamespaceStatusCount {
                namespace,
                status,
                count: count.max(0) as u64,
            })
            .collect(),
        Err(e) => {
            error!(
                "metrics: grouping deployment by namespace/status failed: {}",
                e
            );
            Vec::new()
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
    write_namespace_status_gauge(&mut out, snap);

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

    render_runtime(&mut out, snap);

    out
}

/// Render the per-deployment runtime resource usage section. Sourced from the
/// background cache; values are at most one refresh interval stale, surfaced
/// via `ring_runtime_last_refresh_seconds` so a stale/stalled worker is
/// detectable (`time() - ring_runtime_last_refresh_seconds`).
///
/// CPU, memory, pids and instance counts are gauges (point-in-time). Network
/// and disk byte totals are counters (monotonic, cumulative since the
/// instance started) — `rate(...)` them in PromQL.
fn render_runtime(out: &mut String, snap: &Snapshot) {
    write_gauge(
        out,
        "ring_runtime_last_refresh_seconds",
        "Unix timestamp of the last successful runtime-stats refresh. 0 means never.",
        snap.runtime_last_refresh_seconds,
    );

    if snap.deployment_runtime.is_empty() {
        return;
    }

    // (name, HELP, value extractor). One group per metric: a single HELP/TYPE
    // pair followed by all its series, as the exposition format requires.
    let gauges: [RuntimeMetric; 5] = [
        (
            "ring_deployment_instances",
            "Number of running instances per deployment.",
            |s| s.instances.to_string(),
        ),
        (
            "ring_deployment_cpu_usage_percent",
            "Aggregate CPU usage percent across a deployment's instances.",
            |s| format!("{:.2}", s.cpu_usage_percent),
        ),
        (
            "ring_deployment_memory_usage_bytes",
            "Aggregate memory usage in bytes across a deployment's instances.",
            |s| s.memory_usage_bytes.to_string(),
        ),
        (
            "ring_deployment_memory_limit_bytes",
            "Aggregate memory limit in bytes across a deployment's instances.",
            |s| s.memory_limit_bytes.to_string(),
        ),
        (
            "ring_deployment_pids",
            "Aggregate process/thread count across a deployment's instances.",
            |s| s.pids.to_string(),
        ),
    ];
    for metric in gauges {
        write_runtime_metric_group(out, metric, "gauge", &snap.deployment_runtime);
    }

    let counters: [RuntimeMetric; 5] = [
        (
            "ring_deployment_network_rx_bytes_total",
            "Cumulative bytes received per deployment since instance start.",
            |s| s.network_rx_bytes.to_string(),
        ),
        (
            "ring_deployment_network_tx_bytes_total",
            "Cumulative bytes transmitted per deployment since instance start.",
            |s| s.network_tx_bytes.to_string(),
        ),
        (
            "ring_deployment_disk_read_bytes_total",
            "Cumulative bytes read from disk per deployment since instance start.",
            |s| s.disk_read_bytes.to_string(),
        ),
        (
            "ring_deployment_disk_write_bytes_total",
            "Cumulative bytes written to disk per deployment since instance start.",
            |s| s.disk_write_bytes.to_string(),
        ),
        (
            "ring_deployment_restarts_total",
            "Cumulative restart count across a deployment's instances.",
            |s| s.restarts.to_string(),
        ),
    ];
    for metric in counters {
        write_runtime_metric_group(out, metric, "counter", &snap.deployment_runtime);
    }
}

/// A per-deployment runtime metric: its name, HELP text, and a function
/// extracting its rendered value from one deployment's stats.
type RuntimeMetric = (&'static str, &'static str, fn(&RuntimeSeries) -> String);

/// Emit one metric group: the HELP/TYPE header, then one labelled series per
/// deployment.
fn write_runtime_metric_group(
    out: &mut String,
    (name, help, value): RuntimeMetric,
    metric_type: &str,
    deployments: &[RuntimeSeries],
) {
    let _ = writeln!(out, "# HELP {name} {help}");
    let _ = writeln!(out, "# TYPE {name} {metric_type}");
    for s in deployments {
        let _ = writeln!(
            out,
            "{name}{{deployment=\"{}\",namespace=\"{}\",runtime=\"{}\"}} {}",
            escape_label_value(&s.name),
            escape_label_value(&s.namespace),
            escape_label_value(&s.runtime),
            value(s),
        );
    }
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

/// Render the unhealthy-deployments breakdown as a two-label gauge
/// (`namespace`, `status`). The HELP/TYPE pair is always emitted so the metric
/// is discoverable even when nothing is broken and there are no series.
fn write_namespace_status_gauge(out: &mut String, snap: &Snapshot) {
    let name = "ring_unhealthy_deployments";
    let _ = writeln!(
        out,
        "# HELP {name} Deployments in a non-healthy status, by namespace and status."
    );
    let _ = writeln!(out, "# TYPE {name} gauge");
    for row in &snap.unhealthy_deployments_by_namespace {
        let _ = writeln!(
            out,
            "{name}{{namespace=\"{}\",status=\"{}\"}} {}",
            escape_label_value(&row.namespace),
            escape_label_value(&row.status),
            row.count
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

    #[test]
    fn render_runtime_emits_labelled_series_and_skips_when_empty() {
        // Empty cache: only the freshness gauge, no per-deployment series.
        let body = render_prom(&Snapshot::default());
        assert!(body.contains("ring_runtime_last_refresh_seconds 0"));
        assert!(!body.contains("ring_deployment_cpu_usage_percent"));

        // Populated cache: one deployment, fully labelled, gauges + counters.
        let snap = Snapshot {
            runtime_last_refresh_seconds: 1717000000,
            deployment_runtime: vec![RuntimeSeries {
                name: "web".to_string(),
                namespace: "prod".to_string(),
                runtime: "docker".to_string(),
                instances: 2,
                cpu_usage_percent: 12.5,
                memory_usage_bytes: 1000,
                memory_limit_bytes: 4000,
                network_rx_bytes: 50,
                network_tx_bytes: 60,
                disk_read_bytes: 70,
                disk_write_bytes: 80,
                pids: 9,
                restarts: 3,
            }],
            ..Default::default()
        };
        let body = render_prom(&snap);

        assert!(body.contains("ring_runtime_last_refresh_seconds 1717000000"));
        assert!(body.contains("# TYPE ring_deployment_cpu_usage_percent gauge"));
        assert!(body.contains(
            "ring_deployment_cpu_usage_percent{deployment=\"web\",namespace=\"prod\",runtime=\"docker\"} 12.50"
        ));
        assert!(body.contains(
            "ring_deployment_instances{deployment=\"web\",namespace=\"prod\",runtime=\"docker\"} 2"
        ));
        // Cumulative metrics are counters with the `_total` suffix.
        assert!(body.contains("# TYPE ring_deployment_network_rx_bytes_total counter"));
        assert!(body.contains(
            "ring_deployment_network_rx_bytes_total{deployment=\"web\",namespace=\"prod\",runtime=\"docker\"} 50"
        ));
        assert!(body.contains(
            "ring_deployment_restarts_total{deployment=\"web\",namespace=\"prod\",runtime=\"docker\"} 3"
        ));
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

    async fn insert_deployment(
        pool: &sqlx::SqlitePool,
        id: &str,
        namespace: &str,
        name: &str,
        status: &str,
    ) {
        sqlx::query(
            "INSERT INTO deployment (id, created_at, status, namespace, runtime, kind, name) \
             VALUES (?, '2024-01-01', ?, ?, 'docker', 'worker', ?)",
        )
        .bind(id)
        .bind(status)
        .bind(namespace)
        .bind(name)
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn unhealthy_by_namespace_buckets_only_unhealthy_statuses() {
        let (pool, _app) = new_test_app_with_pool().await;

        // Two tenants, a mix of states. Only the unhealthy ones must surface,
        // each tagged with its own namespace so an operator can tell who is hit.
        // Use synthetic namespaces so the fixtures' own deployments don't bleed
        // into the assertion.
        insert_deployment(&pool, "k1", "ns-trouble-a", "proxysql", "running").await;
        insert_deployment(&pool, "k2", "ns-trouble-a", "pgbouncer", "config_error").await;
        insert_deployment(&pool, "c1", "ns-trouble-b", "web", "crash_loop_back_off").await;
        insert_deployment(&pool, "c2", "ns-trouble-b", "old", "completed").await;
        insert_deployment(&pool, "c3", "ns-trouble-b", "img", "image_pull_back_off").await;

        let rows = unhealthy_by_namespace(&pool, &UNHEALTHY_STATUSES).await;
        // Restrict to the namespaces we created — the fixtures seed their own
        // deployments, so assert on our delta rather than the whole set.
        let got: Vec<(&str, &str, u64)> = rows
            .iter()
            .filter(|r| r.namespace == "ns-trouble-a" || r.namespace == "ns-trouble-b")
            .map(|r| (r.namespace.as_str(), r.status.as_str(), r.count))
            .collect();

        // running + completed are excluded; the three unhealthy ones remain,
        // ordered by (namespace, status).
        assert_eq!(
            got,
            vec![
                ("ns-trouble-a", "config_error", 1),
                ("ns-trouble-b", "crash_loop_back_off", 1),
                ("ns-trouble-b", "image_pull_back_off", 1),
            ]
        );
    }

    #[tokio::test]
    async fn unhealthy_deployments_render_with_namespace_and_status_labels() {
        let (pool, _app) = new_test_app_with_pool().await;
        insert_deployment(&pool, "k2", "ns-trouble-a", "pgbouncer", "config_error").await;
        insert_deployment(&pool, "k1", "ns-trouble-a", "proxysql", "running").await;

        let snap = Snapshot::collect(&pool).await;
        let body = render_prom(&snap);

        assert!(body.contains("# TYPE ring_unhealthy_deployments gauge"));
        assert!(body.contains(
            "ring_unhealthy_deployments{namespace=\"ns-trouble-a\",status=\"config_error\"} 1"
        ));
        // A healthy deployment never appears in *this* metric (the substring is
        // scoped to ring_unhealthy_deployments, not the global by-status gauge).
        assert!(
            !body.contains(
                "ring_unhealthy_deployments{namespace=\"ns-trouble-a\",status=\"running\""
            )
        );
    }
}
