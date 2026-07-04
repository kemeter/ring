//! OpenTelemetry distributed-tracing pipeline (opt-in).
//!
//! When `[server.telemetry.traces]` is enabled, [`build_tracer`] builds an
//! OTLP/gRPC span exporter, wires it into an [`SdkTracerProvider`] with a
//! configurable sampler, and returns a `tracing_subscriber` layer that turns
//! every `tracing` span into an OpenTelemetry span, plus a guard.
//!
//! The guard ([`OtelGuard`]) owns the provider and flushes on drop: dropping it
//! without flushing would lose the last, un-exported batch. Keep it alive for
//! the whole process (bind it in `main`), and it shuts the pipeline down cleanly
//! on exit.
//!
//! Spans are batch-exported. The batch processor (0.28+) spawns its own
//! background worker and no longer takes a runtime argument; Ring runs on a
//! multi-thread tokio runtime, which sidesteps the current-thread shutdown
//! deadlock documented upstream.
//!
//! Failure is non-fatal by design: if the exporter can't be built (bad
//! endpoint, etc.) the caller logs and continues logs-only rather than crash.

use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::WithExportConfig as _;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::trace::{Sampler, SdkTracerProvider};
use tracing::Instrument as _;

use crate::config::server::TracesConfig;

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

/// Keeps the tracer provider alive for the process lifetime and flushes pending
/// spans on shutdown. Drop it (or call [`OtelGuard::shutdown`]) to force-flush
/// the last batch. Held by `main` so the pipeline lives as long as the server.
pub(crate) struct OtelGuard {
    provider: SdkTracerProvider,
}

impl OtelGuard {
    /// Flush and stop the exporter. Idempotent enough for a shutdown path.
    pub(crate) fn shutdown(&self) {
        if let Err(e) = self.provider.force_flush() {
            warn!("telemetry: failed to flush spans on shutdown: {e}");
        }
        if let Err(e) = self.provider.shutdown() {
            warn!("telemetry: failed to shut tracer provider down: {e}");
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

    Ok((layer, OtelGuard { provider }))
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
