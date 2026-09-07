//! Prometheus metrics — Phase 3 of `.claude/plans/observability/PRD.md`.
//!
//! Decision 5: a native pull-based `/metrics` HTTP endpoint per service,
//! deliberately *not* routed through the OTel Collector/exporter Phase 2
//! built — traces go out via OTLP push, metrics stay a separate, simpler
//! pull-based endpoint each service exposes directly. Controlled by
//! [`PORT_ENV_VAR`]; unset means no HTTP server is ever bound — genuinely
//! zero overhead, not just an unused code path.
//!
//! Decision 6: RED-method metrics (`coordin8_grpc_requests_total`,
//! `coordin8_grpc_request_duration_seconds`) recorded once per RPC by the
//! same [`crate::server_layer`] Phase 2 already applies to every service,
//! plus process-level CPU/RAM/fd metrics via [`prometheus`]'s built-in
//! `ProcessCollector` (Linux only — reads `/proc`; a no-op registration
//! elsewhere, e.g. local macOS dev). **Cardinality rule**: labels are
//! `grpc_service`/`grpc_method`/`grpc_code` only — a small, bounded set
//! derived from the RPC method name and gRPC status, never a tuple ID,
//! capability ID, lease ID, or any other unbounded value.

use std::sync::LazyLock;
use std::time::Duration;

use prometheus::{Encoder, HistogramVec, IntCounterVec, Registry, TextEncoder};
use tower_http::classify::GrpcFailureClass;
use tracing::Span;

/// Env var for the `/metrics` HTTP port. Unset means metrics are disabled
/// entirely — no HTTP server is bound.
pub const PORT_ENV_VAR: &str = "COORDIN8_METRICS_PORT";

static REGISTRY: LazyLock<Registry> = LazyLock::new(|| {
    let registry = Registry::new();
    register_process_collector(&registry);
    registry
});

static REQUESTS_TOTAL: LazyLock<IntCounterVec> = LazyLock::new(|| {
    let counter = IntCounterVec::new(
        prometheus::Opts::new(
            "coordin8_grpc_requests_total",
            "Total gRPC requests handled, labeled by service, method, and gRPC status code",
        ),
        &["grpc_service", "grpc_method", "grpc_code"],
    )
    .expect("static metric definition is valid");
    REGISTRY
        .register(Box::new(counter.clone()))
        .expect("registered exactly once, at first use");
    counter
});

static REQUEST_DURATION: LazyLock<HistogramVec> = LazyLock::new(|| {
    let histogram = HistogramVec::new(
        prometheus::HistogramOpts::new(
            "coordin8_grpc_request_duration_seconds",
            "gRPC request duration in seconds, labeled by service and method",
        ),
        &["grpc_service", "grpc_method"],
    )
    .expect("static metric definition is valid");
    REGISTRY
        .register(Box::new(histogram.clone()))
        .expect("registered exactly once, at first use");
    histogram
});

#[cfg(target_os = "linux")]
fn register_process_collector(registry: &Registry) {
    let collector = prometheus::process_collector::ProcessCollector::for_self();
    if let Err(e) = registry.register(Box::new(collector)) {
        tracing::warn!("failed to register process metrics collector: {e}");
    }
}

/// Process-level metrics (`process_cpu_seconds_total`,
/// `process_resident_memory_bytes`, etc.) come from reading `/proc`, so
/// they're Linux-only — a real gap on other platforms, not worth a
/// userspace-`sysinfo`-based substitute for what is, for every deployment
/// target this project actually ships to (Docker), a Linux container
/// anyway.
#[cfg(not(target_os = "linux"))]
fn register_process_collector(_registry: &Registry) {}

/// The (service, method) a `grpc_request` span was created for — stashed
/// as a span extension in [`crate::otel::make_span`] so the `on_response`/
/// `on_failure` hooks below can label metrics correctly. Neither hook
/// receives the original request, only the response/failure and the span
/// itself, so this is the standard way to carry per-request data between
/// `TraceLayer` callbacks (the same mechanism `tracing-opentelemetry`
/// itself uses for `OtelData`).
pub(crate) struct GrpcLabels {
    pub service: String,
    pub method: String,
}

fn grpc_code_label(class: &GrpcFailureClass) -> String {
    match class {
        GrpcFailureClass::Status(status) => match status.code() {
            Some(code) => format!("{code:?}"),
            None => format!("unrecognized({})", status.code_raw()),
        },
        // Deliberately not including the error text here — it's
        // unbounded-cardinality free text (a transport-level failure
        // description), which the module docs' cardinality rule rules out
        // as a label value.
        GrpcFailureClass::Error(_) => "transport_error".to_string(),
        _ => "unknown".to_string(),
    }
}

/// Stashes `service`/`method` as a typed extension on `span`, reachable
/// later (from a *different* callback) via [`labels_from_span`]. A plain
/// `tracing::Span` has no public `extensions_mut()` — only
/// `tracing_subscriber`'s `SpanRef` (reached through the registry) does,
/// so this goes through `Span::with_subscriber` and downcasts to the
/// concrete `Registry` type `crate::init` installs. The same mechanism
/// `tracing-opentelemetry` itself uses internally for `OtelData`.
pub(crate) fn stash_grpc_labels(span: &Span, service: String, method: String) {
    span.with_subscriber(|(id, subscriber)| {
        if let Some(registry) = subscriber.downcast_ref::<tracing_subscriber::Registry>() {
            use tracing_subscriber::registry::LookupSpan;
            if let Some(span_ref) = registry.span(id) {
                span_ref
                    .extensions_mut()
                    .insert(GrpcLabels { service, method });
            }
        }
    });
}

fn labels_from_span(span: &Span) -> (String, String) {
    span.with_subscriber(|(id, subscriber)| {
        subscriber
            .downcast_ref::<tracing_subscriber::Registry>()
            .and_then(|registry| {
                use tracing_subscriber::registry::LookupSpan;
                registry.span(id)
            })
            .and_then(|span_ref| {
                span_ref
                    .extensions()
                    .get::<GrpcLabels>()
                    .map(|l| (l.service.clone(), l.method.clone()))
            })
    })
    .flatten()
    .unwrap_or_else(|| ("unknown".to_string(), "unknown".to_string()))
}

/// [`tower_http::trace::TraceLayer`]'s `on_response` hook. Every response
/// reaching this point is gRPC-OK by the classifier's own definition
/// (`GrpcErrorsAsFailures`) — non-OK responses go through [`on_failure`]
/// instead — so this always records the `Ok` code.
pub(crate) fn on_response<B>(_response: &http::Response<B>, latency: Duration, span: &Span) {
    let (service, method) = labels_from_span(span);
    REQUESTS_TOTAL
        .with_label_values(&[service.as_str(), method.as_str(), "Ok"])
        .inc();
    REQUEST_DURATION
        .with_label_values(&[service.as_str(), method.as_str()])
        .observe(latency.as_secs_f64());
}

/// [`tower_http::trace::TraceLayer`]'s `on_failure` hook — the
/// classifier's `Status`/`Error` split, not a raw response, so it labels
/// with the actual gRPC status rather than always `Ok`.
pub(crate) fn on_failure(class: GrpcFailureClass, latency: Duration, span: &Span) {
    let (service, method) = labels_from_span(span);
    let code = grpc_code_label(&class);
    REQUESTS_TOTAL
        .with_label_values(&[service.as_str(), method.as_str(), code.as_str()])
        .inc();
    REQUEST_DURATION
        .with_label_values(&[service.as_str(), method.as_str()])
        .observe(latency.as_secs_f64());
}

/// Splits a gRPC request path (`"/coordin8.RegistryService/LookupAll"`)
/// into `(service, method)`. Falls back to `("unknown", <whole path>)` for
/// anything that doesn't match the expected shape — should never happen
/// for real gRPC traffic, but a metrics label must never panic on
/// malformed input.
pub(crate) fn split_grpc_path(path: &str) -> (String, String) {
    let trimmed = path.trim_start_matches('/');
    match trimmed.split_once('/') {
        Some((service, method)) => (service.to_string(), method.to_string()),
        None => ("unknown".to_string(), trimmed.to_string()),
    }
}

/// Renders the current metrics snapshot in Prometheus text-exposition
/// format — what the `/metrics` HTTP endpoint serves.
pub(crate) fn render() -> Vec<u8> {
    let metric_families = REGISTRY.gather();
    let encoder = TextEncoder::new();
    let mut buf = Vec::new();
    let _ = encoder.encode(&metric_families, &mut buf);
    buf
}

/// Metrics configuration for one service, built from the environment —
/// matches every other Coordin8 knob. The only effect of this config is
/// whether [`MetricsConfig::spawn`] actually binds a port.
pub struct MetricsConfig {
    port: Option<u16>,
}

impl MetricsConfig {
    pub fn from_env() -> Self {
        let port = std::env::var(PORT_ENV_VAR)
            .ok()
            .and_then(|v| v.parse().ok());
        Self { port }
    }

    pub fn enabled(&self) -> bool {
        self.port.is_some()
    }

    /// Spawns the `/metrics` HTTP server as a background task if
    /// [`PORT_ENV_VAR`] is set; does nothing (returns `None`, binds no
    /// port at all) otherwise — Decision 5's zero-overhead-when-disabled
    /// guarantee.
    pub fn spawn(&self) -> Option<tokio::task::JoinHandle<()>> {
        let port = self.port?;
        Some(tokio::spawn(serve(port)))
    }
}

async fn serve(port: u16) {
    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(listener) => listener,
        Err(e) => {
            tracing::error!("metrics server failed to bind {addr}: {e}");
            return;
        }
    };
    tracing::info!("metrics server listening on {addr}");

    loop {
        let (stream, _) = match listener.accept().await {
            Ok(pair) => pair,
            Err(e) => {
                tracing::warn!("metrics server accept failed: {e}");
                continue;
            }
        };
        let io = hyper_util::rt::TokioIo::new(stream);
        tokio::spawn(async move {
            let service = hyper::service::service_fn(handle_request);
            if let Err(e) = hyper::server::conn::http1::Builder::new()
                .serve_connection(io, service)
                .await
            {
                tracing::debug!("metrics connection error: {e}");
            }
        });
    }
}

/// Routes the metrics HTTP server: `GET /metrics` returns the current
/// snapshot, anything else 404s or 405s rather than dumping the full
/// metrics body for every request regardless of path/verb. Deliberately
/// unauthenticated — standard for a Prometheus scrape target — so this is
/// the one thing keeping it from being "any request gets everything."
async fn handle_request(
    req: hyper::Request<hyper::body::Incoming>,
) -> Result<hyper::Response<http_body_util::Full<bytes::Bytes>>, std::convert::Infallible> {
    if req.uri().path() != "/metrics" {
        return Ok(hyper::Response::builder()
            .status(hyper::StatusCode::NOT_FOUND)
            .body(http_body_util::Full::new(bytes::Bytes::new()))
            .expect("static response is always valid"));
    }
    if req.method() != hyper::Method::GET {
        return Ok(hyper::Response::builder()
            .status(hyper::StatusCode::METHOD_NOT_ALLOWED)
            .body(http_body_util::Full::new(bytes::Bytes::new()))
            .expect("static response is always valid"));
    }

    let body = render();
    Ok(hyper::Response::builder()
        .header("content-type", "text/plain; version=0.0.4")
        .body(http_body_util::Full::new(bytes::Bytes::from(body)))
        .expect("static response is always valid"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_grpc_path_splits_service_and_method() {
        assert_eq!(
            split_grpc_path("/coordin8.RegistryService/LookupAll"),
            (
                "coordin8.RegistryService".to_string(),
                "LookupAll".to_string()
            )
        );
    }

    #[test]
    fn split_grpc_path_falls_back_on_malformed_input() {
        assert_eq!(
            split_grpc_path("nonsense"),
            ("unknown".to_string(), "nonsense".to_string())
        );
        assert_eq!(split_grpc_path(""), ("unknown".to_string(), "".to_string()));
    }

    #[test]
    fn metrics_config_disabled_when_port_unset() {
        assert!(!MetricsConfig { port: None }.enabled());
    }

    #[test]
    fn metrics_config_enabled_when_port_set() {
        assert!(MetricsConfig { port: Some(9100) }.enabled());
    }

    #[test]
    fn grpc_code_label_never_leaks_free_text_error_into_the_label() {
        let class = GrpcFailureClass::Error("some very specific transport failure".to_string());
        assert_eq!(grpc_code_label(&class), "transport_error");
    }

    #[test]
    fn labels_from_span_defaults_to_unknown_without_a_registry_subscriber() {
        // No registry subscriber installed in this test's scope (unlike
        // otel::tests, which installs one) — exercises the fallback path
        // rather than the happy path, which is covered by the live
        // grpcurl verification against a real Djinn.
        let span = tracing::Span::none();
        assert_eq!(
            labels_from_span(&span),
            ("unknown".to_string(), "unknown".to_string())
        );
    }
}
