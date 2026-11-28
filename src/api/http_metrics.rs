//! HTTP server request duration, per `(method, route, status)`.
//!
//! [`http_trace_middleware`](crate::telemetry::http_trace_middleware) times
//! every request and records it here. The histogram is served two ways:
//!
//! - in Prometheus text on `/metrics`, as
//!   `ring_http_server_request_duration_seconds` (always on, kept in memory);
//! - over OTLP as `http.server.request.duration` (the OpenTelemetry semantic
//!   convention name), once [`HttpMetrics::attach_otel`] has been called, which
//!   happens only when `[server.telemetry.metrics]` is enabled.
//!
//! Labels stay low-cardinality whatever the client sends: the route is the
//! matched template (`/deployments/{id}`, never the raw path, and empty when no
//! route matched), and a method outside the standard set is recorded as
//! `_OTHER`, as the semantic convention prescribes.
//!
//! The duration runs until the response head is ready. For the streaming
//! routes (logs, exec) that is the time to open the stream, not its lifetime.

use opentelemetry::KeyValue;
use opentelemetry::metrics::{Histogram, Meter};
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};

/// Upper bounds of the buckets, in seconds: the OpenTelemetry default advice
/// for `http.server.request.duration`, so both exports bucket alike.
pub(crate) const DURATION_BUCKETS: [f64; 14] = [
    0.005, 0.01, 0.025, 0.05, 0.075, 0.1, 0.25, 0.5, 0.75, 1.0, 2.5, 5.0, 7.5, 10.0,
];

/// Methods kept as they are; any other is recorded as `_OTHER`.
const KNOWN_METHODS: [&str; 9] = [
    "GET", "HEAD", "POST", "PUT", "DELETE", "CONNECT", "OPTIONS", "TRACE", "PATCH",
];

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct SeriesKey {
    method: String,
    route: String,
    status: u16,
}

#[derive(Clone, Debug, Default)]
struct Series {
    /// Non-cumulative: `buckets[i]` counts durations in
    /// `(DURATION_BUCKETS[i-1], DURATION_BUCKETS[i]]`. Durations above the last
    /// bound only count in `count`.
    buckets: [u64; DURATION_BUCKETS.len()],
    count: u64,
    sum_seconds: f64,
}

#[derive(Default)]
struct Inner {
    series: Mutex<BTreeMap<SeriesKey, Series>>,
    otel: OnceLock<Histogram<f64>>,
}

/// Shared handle on the request duration histogram. Cloning is cheap (an
/// `Arc`); every clone records into the same series.
#[derive(Clone, Default)]
pub(crate) struct HttpMetrics(Arc<Inner>);

/// One series, ready to serialize in the JSON form of `/metrics`.
#[derive(Debug, Serialize)]
pub(crate) struct HttpSeriesSnapshot {
    pub(crate) method: String,
    pub(crate) route: String,
    pub(crate) status_code: u16,
    pub(crate) count: u64,
    pub(crate) sum_seconds: f64,
    /// Cumulative counts per upper bound, as in Prometheus: the requests that
    /// took at most `le` seconds.
    pub(crate) buckets: Vec<BucketSnapshot>,
}

#[derive(Debug, Serialize)]
pub(crate) struct BucketSnapshot {
    pub(crate) le: f64,
    pub(crate) count: u64,
}

impl HttpMetrics {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Also export every request recorded from now on over OTLP, through
    /// `meter`. Only the first call has an effect.
    pub(crate) fn attach_otel(&self, meter: &Meter) {
        let histogram = meter
            .f64_histogram("http.server.request.duration")
            .with_unit("s")
            .with_description("Duration of HTTP server requests.")
            .with_boundaries(DURATION_BUCKETS.to_vec())
            .build();
        let _ = self.0.otel.set(histogram);
    }

    /// Record one request. `route` is the matched route template, `None` when
    /// no route matched.
    pub(crate) fn record(&self, method: &str, route: Option<&str>, status: u16, seconds: f64) {
        let method = normalize_method(method);

        if let Some(histogram) = self.0.otel.get() {
            let mut attributes = vec![
                KeyValue::new("http.request.method", method),
                KeyValue::new("http.response.status_code", i64::from(status)),
            ];
            if let Some(route) = route {
                attributes.push(KeyValue::new("http.route", route.to_string()));
            }
            histogram.record(seconds, &attributes);
        }

        let key = SeriesKey {
            method: method.to_string(),
            route: route.unwrap_or_default().to_string(),
            status,
        };
        // A poisoned lock only means another request panicked mid-record; the
        // counters are still usable, so keep recording rather than lose them.
        let mut series = self
            .0
            .series
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = series.entry(key).or_default();
        entry.count += 1;
        entry.sum_seconds += seconds;
        if let Some(i) = DURATION_BUCKETS.iter().position(|&le| seconds <= le) {
            entry.buckets[i] += 1;
        }
    }

    /// Every series recorded so far, sorted by method, route and status.
    pub(crate) fn snapshot(&self) -> Vec<HttpSeriesSnapshot> {
        let series = self
            .0
            .series
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        series
            .iter()
            .map(|(key, s)| {
                let mut cumulative = 0;
                let buckets = DURATION_BUCKETS
                    .iter()
                    .zip(s.buckets)
                    .map(|(&le, n)| {
                        cumulative += n;
                        BucketSnapshot {
                            le,
                            count: cumulative,
                        }
                    })
                    .collect();
                HttpSeriesSnapshot {
                    method: key.method.clone(),
                    route: key.route.clone(),
                    status_code: key.status,
                    count: s.count,
                    sum_seconds: s.sum_seconds,
                    buckets,
                }
            })
            .collect()
    }
}

fn normalize_method(method: &str) -> &'static str {
    KNOWN_METHODS
        .iter()
        .find(|known| **known == method)
        .copied()
        .unwrap_or("_OTHER")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_into_the_first_bucket_that_holds_the_duration() {
        let metrics = HttpMetrics::new();
        metrics.record("GET", Some("/deployments"), 200, 0.004);
        metrics.record("GET", Some("/deployments"), 200, 0.03);
        metrics.record("GET", Some("/deployments"), 200, 0.05);
        metrics.record("GET", Some("/deployments"), 200, 42.0);

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.len(), 1);
        let series = &snapshot[0];
        assert_eq!(series.count, 4);
        assert!((series.sum_seconds - 42.084).abs() < 1e-9);

        let at = |le: f64| {
            series
                .buckets
                .iter()
                .find(|b| (b.le - le).abs() < f64::EPSILON)
                .unwrap()
                .count
        };
        assert_eq!(at(0.005), 1);
        assert_eq!(at(0.025), 1);
        // 0.05 is inclusive: Prometheus buckets are `le`.
        assert_eq!(at(0.05), 3);
        // 42 s is above the last bound: only in `count`, the `+Inf` bucket.
        assert_eq!(at(10.0), 3);
    }

    #[test]
    fn keeps_one_series_per_method_route_and_status() {
        let metrics = HttpMetrics::new();
        metrics.record("GET", Some("/deployments/{id}"), 200, 0.01);
        metrics.record("GET", Some("/deployments/{id}"), 404, 0.01);
        metrics.record("DELETE", Some("/deployments/{id}"), 200, 0.01);
        metrics.record("GET", Some("/deployments/{id}"), 200, 0.01);

        let snapshot = metrics.snapshot();
        let summary: Vec<_> = snapshot
            .iter()
            .map(|s| (s.method.as_str(), s.status_code, s.count))
            .collect();
        assert_eq!(
            summary,
            vec![("DELETE", 200, 1), ("GET", 200, 2), ("GET", 404, 1)]
        );
    }

    #[test]
    fn unknown_methods_and_unmatched_routes_do_not_add_labels() {
        let metrics = HttpMetrics::new();
        metrics.record("BREW", None, 404, 0.001);
        metrics.record("PROPFIND", None, 404, 0.001);

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].method, "_OTHER");
        assert_eq!(snapshot[0].route, "");
        assert_eq!(snapshot[0].count, 2);
    }
}
