//! OpenTelemetry export pipeline (opt-in, one signal per sub-block).
//!
//! Three independent signals, each built only when its `[server.telemetry.*]`
//! block is enabled:
//!
//! - **traces** — [`build_tracer`] builds an OTLP/gRPC span exporter behind an
//!   [`SdkTracerProvider`] with a configurable sampler, and returns a
//!   `tracing_subscriber` layer that turns every `tracing` span into an OTel
//!   span.
//! - **metrics** — [`build_meter`] builds an [`SdkMeterProvider`] with a
//!   periodic OTLP push reader and an observable callback that reads the
//!   background stats cache (no extra runtime round-trip) and reports the
//!   per-deployment resource gauges the Prometheus `/metrics` endpoint already
//!   serves.
//! - **logs** — [`build_logger`] builds an [`SdkLoggerProvider`] and returns an
//!   `opentelemetry-appender-tracing` layer that ships `tracing` events over
//!   OTLP in addition to the console.
//!
//! The guard ([`OtelGuard`]) owns whichever providers were built and flushes
//! them on drop: dropping without flushing would lose the last, un-exported
//! batch. Keep it alive for the whole process (bind it in `main`), and it shuts
//! every pipeline down cleanly on exit.
//!
//! Spans and logs are batch-exported; metrics are pushed on a periodic reader.
//! The batch processor (0.28+) spawns its own background worker and no longer
//! takes a runtime argument; Ring runs on a multi-thread tokio runtime, which
//! sidesteps the current-thread shutdown deadlock documented upstream.
//!
//! Failure is non-fatal by design: if an exporter can't be built (bad endpoint,
//! etc.) the caller logs and continues without that signal rather than crash.

use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::WithExportConfig as _;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::metrics::{PeriodicReader, SdkMeterProvider};
use opentelemetry_sdk::trace::{Sampler, SdkTracerProvider};
use std::time::Duration;
use tracing::Instrument as _;

use crate::config::server::{LogsConfig, MetricsConfig, TracesConfig};
use crate::scheduler::stats_cache::StatsCache;

/// Parse the `sampler` config string into an OpenTelemetry [`Sampler`].
///
/// - `parent_based_always_on` (default): honour an upstream sampling decision,
///   otherwise sample. The right server default — if a caller is tracing, we
///   continue the trace; unparented roots are sampled.
/// - `always_on` / `always_off`: force the decision.
/// - `parent_based_always_off`: honour the parent, drop unparented roots.
/// - `ratio:<0..1>`: parent-based ratio sampler, e.g. `ratio:0.1` = 10% of
///   roots.
///
/// An unrecognised value falls back to `parent_based_always_on` and logs once.
fn parse_sampler(spec: &str) -> Sampler {
    match spec.trim().to_ascii_lowercase().as_str() {
        "always_on" => Sampler::AlwaysOn,
        "always_off" => Sampler::AlwaysOff,
        "parent_based_always_on" => Sampler::ParentBased(Box::new(Sampler::AlwaysOn)),
        "parent_based_always_off" => Sampler::ParentBased(Box::new(Sampler::AlwaysOff)),
        other => {
            if let Some(rest) = other.strip_prefix("ratio:")
                && let Ok(ratio) = rest.parse::<f64>()
            {
                let ratio = ratio.clamp(0.0, 1.0);
                return Sampler::ParentBased(Box::new(Sampler::TraceIdRatioBased(ratio)));
            }
            warn!("telemetry: unknown sampler '{spec}', using parent_based_always_on");
            Sampler::ParentBased(Box::new(Sampler::AlwaysOn))
        }
    }
}

/// Keeps whichever OTel providers were built alive for the process lifetime and
/// flushes them on shutdown. Drop it (or call [`OtelGuard::shutdown`]) to
/// force-flush the last batch of each signal. Held by `main` so the pipelines
/// live as long as the server. Each provider is optional: a signal left
/// disabled simply leaves its slot `None`.
#[derive(Default)]
pub(crate) struct OtelGuard {
    tracer: Option<SdkTracerProvider>,
    meter: Option<SdkMeterProvider>,
    logger: Option<opentelemetry_sdk::logs::SdkLoggerProvider>,
}

impl OtelGuard {
    /// Move another guard's tracer provider into this one. Used by `init_tracing`
    /// to fold the guard [`build_tracer`] returns into the process-wide guard.
    pub(crate) fn absorb(&mut self, other: OtelGuard) {
        let mut other = other;
        if let Some(p) = other.tracer.take() {
            self.tracer = Some(p);
        }
        if let Some(p) = other.meter.take() {
            self.meter = Some(p);
        }
        if let Some(p) = other.logger.take() {
            self.logger = Some(p);
        }
        // `other` is dropped here, but its providers have been moved out, so its
        // `Drop` shuts nothing down.
    }

    /// Attach the logger provider so the guard flushes it on shutdown.
    pub(crate) fn attach_logger(&mut self, provider: opentelemetry_sdk::logs::SdkLoggerProvider) {
        self.logger = Some(provider);
    }

    /// Attach the meter provider (built later, once the stats cache exists) so
    /// the guard flushes it on shutdown.
    pub(crate) fn attach_meter(&mut self, provider: SdkMeterProvider) {
        self.meter = Some(provider);
    }

    /// Flush and stop every built exporter. Idempotent enough for a shutdown
    /// path; a signal that was never enabled is skipped.
    pub(crate) fn shutdown(&self) {
        if let Some(p) = &self.tracer {
            if let Err(e) = p.force_flush() {
                warn!("telemetry: failed to flush spans on shutdown: {e}");
            }
            if let Err(e) = p.shutdown() {
                warn!("telemetry: failed to shut tracer provider down: {e}");
            }
        }
        if let Some(p) = &self.meter {
            if let Err(e) = p.force_flush() {
                warn!("telemetry: failed to flush metrics on shutdown: {e}");
            }
            if let Err(e) = p.shutdown() {
                warn!("telemetry: failed to shut meter provider down: {e}");
            }
        }
        if let Some(p) = &self.logger {
            if let Err(e) = p.force_flush() {
                warn!("telemetry: failed to flush logs on shutdown: {e}");
            }
            if let Err(e) = p.shutdown() {
                warn!("telemetry: failed to shut logger provider down: {e}");
            }
        }
    }
}

impl Drop for OtelGuard {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Build the OTLP tracing pipeline and return `(layer, guard)`.
///
/// The layer must be added to the `tracing_subscriber` registry; the guard must
/// be kept alive for the process. Returns `Err` if the OTLP exporter can't be
/// built so the caller can fall back to logs-only rather than crash. Endpoint
/// and service name are resolved with `OTEL_*` env overriding the TOML values.
pub(crate) fn build_tracer<S>(
    cfg: &TracesConfig,
) -> anyhow::Result<(
    tracing_opentelemetry::OpenTelemetryLayer<S, opentelemetry_sdk::trace::SdkTracer>,
    OtelGuard,
)>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .with_endpoint(cfg.resolved_endpoint())
        .build()?;

    let resource = Resource::builder()
        .with_service_name(cfg.resolved_service_name())
        .build();

    let provider = SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_sampler(parse_sampler(&cfg.sampler))
        .with_resource(resource)
        .build();

    let tracer = provider.tracer("ring-server");
    let layer = tracing_opentelemetry::layer().with_tracer(tracer);

    Ok((
        layer,
        OtelGuard {
            tracer: Some(provider),
            meter: None,
            logger: None,
        },
    ))
}

/// Build the OTLP metrics pipeline: an [`SdkMeterProvider`] with a periodic push
/// reader and an observable callback that reads the shared stats cache and
/// reports the per-deployment runtime gauges. Returns the provider (to be held
/// by the [`OtelGuard`]) or `Err` if the exporter can't be built so the caller
/// can continue without metrics rather than crash.
///
/// No new runtime work is done here: the callback reads the same in-memory
/// `StatsSnapshot` the background stats cache refreshes for the Prometheus
/// `/metrics` endpoint, so enabling OTLP metrics adds no runtime round-trips.
pub(crate) fn build_meter(
    cfg: &MetricsConfig,
    stats: StatsCache,
) -> anyhow::Result<SdkMeterProvider> {
    let exporter = opentelemetry_otlp::MetricExporter::builder()
        .with_tonic()
        .with_endpoint(cfg.resolved_endpoint())
        .build()?;

    let reader = PeriodicReader::builder(exporter)
        .with_interval(Duration::from_secs(cfg.resolved_interval_seconds()))
        .build();

    let resource = Resource::builder()
        .with_service_name(cfg.resolved_service_name())
        .build();

    let provider = SdkMeterProvider::builder()
        .with_reader(reader)
        .with_resource(resource)
        .build();

    register_deployment_gauges(&provider, stats);

    Ok(provider)
}

/// Register the per-deployment resource gauges on `provider`. Each is an
/// observable gauge whose callback snapshots the stats cache and emits one
/// measurement per deployment, labelled `{deployment, namespace, runtime}` (the
/// same label set the Prometheus renderer uses).
///
/// A poisoned cache lock yields no measurements for that collection cycle
/// (logged) rather than panicking the exporter thread.
fn register_deployment_gauges(provider: &SdkMeterProvider, stats: StatsCache) {
    use opentelemetry::KeyValue;
    use opentelemetry::metrics::MeterProvider as _;

    let meter = provider.meter("ring-server");

    // One helper per numeric field. Each gauge clones the `Arc` cache handle and
    // the field extractor into its callback. `f64` gauges keep a single
    // instrument type across integer byte counts and the CPU percentage.
    macro_rules! deployment_gauge {
        ($name:expr, $unit:expr, $desc:expr, $extract:expr) => {{
            let stats = stats.clone();
            let extract: fn(&crate::scheduler::stats_cache::DeploymentRuntimeStats) -> f64 =
                $extract;
            meter
                .f64_observable_gauge($name)
                .with_unit($unit)
                .with_description($desc)
                .with_callback(move |observer| match stats.read() {
                    Ok(snap) => {
                        for d in &snap.deployments {
                            observer.observe(
                                extract(d),
                                &[
                                    KeyValue::new("deployment", d.name.clone()),
                                    KeyValue::new("namespace", d.namespace.clone()),
                                    KeyValue::new("runtime", d.runtime.clone()),
                                ],
                            );
                        }
                    }
                    Err(e) => {
                        warn!("telemetry: stats cache lock poisoned in metrics callback: {e}")
                    }
                })
                .build();
        }};
    }

    deployment_gauge!(
        "ring.deployment.cpu.percent",
        "percent",
        "Total CPU usage across the deployment's instances",
        |d| d.cpu_usage_percent
    );
    deployment_gauge!(
        "ring.deployment.memory.used_bytes",
        "By",
        "Total memory used across the deployment's instances",
        |d| d.memory_usage_bytes as f64
    );
    deployment_gauge!(
        "ring.deployment.memory.limit_bytes",
        "By",
        "Total memory limit across the deployment's instances",
        |d| d.memory_limit_bytes as f64
    );
    deployment_gauge!(
        "ring.deployment.network.rx_bytes",
        "By",
        "Total network bytes received across the deployment's instances",
        |d| d.network_rx_bytes as f64
    );
    deployment_gauge!(
        "ring.deployment.network.tx_bytes",
        "By",
        "Total network bytes transmitted across the deployment's instances",
        |d| d.network_tx_bytes as f64
    );
    deployment_gauge!(
        "ring.deployment.disk.read_bytes",
        "By",
        "Total disk bytes read across the deployment's instances",
        |d| d.disk_read_bytes as f64
    );
    deployment_gauge!(
        "ring.deployment.disk.write_bytes",
        "By",
        "Total disk bytes written across the deployment's instances",
        |d| d.disk_write_bytes as f64
    );
    deployment_gauge!(
        "ring.deployment.pids",
        "{pid}",
        "Total process/thread count across the deployment's instances",
        |d| d.pids as f64
    );
    deployment_gauge!(
        "ring.deployment.instances",
        "{instance}",
        "Number of live instances in the deployment",
        |d| d.instance_count as f64
    );
    deployment_gauge!(
        "ring.deployment.restarts",
        "{restart}",
        "Total restart count across the deployment's instances",
        |d| d.restarts as f64
    );
}

/// Build the OTLP logs pipeline: an [`SdkLoggerProvider`] with a batch exporter
/// and an `opentelemetry-appender-tracing` layer that bridges `tracing` events
/// to OTLP. Returns `(layer, provider)` or `Err` if the exporter can't be built
/// so the caller can continue console-only rather than crash.
pub(crate) fn build_logger(
    cfg: &LogsConfig,
) -> anyhow::Result<(
    opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge<
        opentelemetry_sdk::logs::SdkLoggerProvider,
        opentelemetry_sdk::logs::SdkLogger,
    >,
    opentelemetry_sdk::logs::SdkLoggerProvider,
)> {
    let exporter = opentelemetry_otlp::LogExporter::builder()
        .with_tonic()
        .with_endpoint(cfg.resolved_endpoint())
        .build()?;

    let resource = Resource::builder()
        .with_service_name(cfg.resolved_service_name())
        .build();

    let provider = opentelemetry_sdk::logs::SdkLoggerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(resource)
        .build();

    let layer = opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge::new(&provider);

    Ok((layer, provider))
}

/// axum middleware: open one `tracing` span per HTTP request, carrying the
/// stable OpenTelemetry HTTP-server semantic-convention attributes. Attached to
/// the root router, so when the OTel layer is active each request becomes a
/// span; when it isn't, this is just a cheap `tracing` span with no exporter.
///
/// The span name is `{method} {http.route}` (low cardinality — the matched
/// route template, never the raw path). Attributes follow the semconv stable
/// set: `http.request.method`, `url.path`, `http.route`,
/// `http.response.status_code`.
pub(crate) async fn http_trace_middleware(
    matched: Option<axum::extract::MatchedPath>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    // `http.route` is the low-cardinality template (e.g. `/users/{id}`); fall
    // back to the raw path only when no route matched.
    let route = matched
        .as_ref()
        .map(|m| m.as_str().to_string())
        .unwrap_or_else(|| path.clone());

    let span = info_span!(
        "http.server.request",
        otel.name = %format!("{method} {route}"),
        otel.kind = "server",
        http.request.method = %method,
        url.path = %path,
        http.route = %route,
        http.response.status_code = tracing::field::Empty,
    );

    // Attach the span to the future rather than holding an `enter()` guard
    // across `.await` (which would leak the span onto whatever task the
    // executor parks on at the yield point).
    async move {
        let response = next.run(request).await;
        tracing::Span::current().record(
            "http.response.status_code",
            response.status().as_u16() as i64,
        );
        response
    }
    .instrument(span)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sampler_parsing_known_values() {
        assert!(matches!(parse_sampler("always_on"), Sampler::AlwaysOn));
        assert!(matches!(parse_sampler("always_off"), Sampler::AlwaysOff));
        assert!(matches!(
            parse_sampler("parent_based_always_on"),
            Sampler::ParentBased(_)
        ));
        assert!(matches!(
            parse_sampler("parent_based_always_off"),
            Sampler::ParentBased(_)
        ));
        assert!(matches!(
            parse_sampler("ratio:0.1"),
            Sampler::ParentBased(_)
        ));
    }

    #[test]
    fn sampler_is_case_and_whitespace_insensitive() {
        assert!(matches!(parse_sampler("  ALWAYS_ON  "), Sampler::AlwaysOn));
    }

    #[test]
    fn unknown_sampler_falls_back_to_parent_based() {
        assert!(matches!(parse_sampler("nonsense"), Sampler::ParentBased(_)));
    }

    #[test]
    fn ratio_out_of_range_is_clamped_not_panicking() {
        // Must not panic; clamping happens internally.
        let _ = parse_sampler("ratio:5.0");
        let _ = parse_sampler("ratio:-1");
        let _ = parse_sampler("ratio:notanumber");
    }
}
