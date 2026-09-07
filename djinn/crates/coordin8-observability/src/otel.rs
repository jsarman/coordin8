//! OpenTelemetry tracing setup — Phase 2 of
//! `.claude/plans/observability/PRD.md`.
//!
//! Decision 3 of that PRD: span creation and `traceparent` propagation run
//! *unconditionally* — that's what makes distributed traces exist at all,
//! and the cost is negligible. Only *exporting* anything to an OTLP
//! collector is gated behind [`ENDPOINT_ENV_VAR`]. To make that true in
//! code (not just in the docs), the tracer provider always exists and the
//! `tracing-opentelemetry` bridge layer is always installed — when no
//! endpoint is configured, the provider is backed by [`NoopExporter`]
//! instead of being entirely absent. This matters because
//! `tracing_opentelemetry::OpenTelemetrySpanExt` (`set_parent`/`context`,
//! used for propagation) only works when that bridge layer is actually
//! registered — without it, propagation would silently no-op even though
//! `tracing::Span`s themselves would still exist.

use std::future::Future;

use opentelemetry::trace::{TraceContextExt, TracerProvider as _};
use opentelemetry::{global, KeyValue};
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::error::OTelSdkResult;
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::{SdkTracerProvider, SpanData, SpanExporter};
use opentelemetry_sdk::Resource;
use tracing_opentelemetry::OpenTelemetrySpanExt;

use crate::propagation::{HeaderExtractor, MetadataInjector};

/// Env var pointing at an OTLP/gRPC collector endpoint (e.g.
/// `http://localhost:4317`). Unset means tracing spans still exist
/// (propagation keeps working) but nothing is ever exported anywhere.
pub const ENDPOINT_ENV_VAR: &str = "COORDIN8_OTEL_ENDPOINT";

/// Env var for the trace sampling ratio, `0.0`-`1.0`. Defaults to `1.0`
/// (always sample) — pre-1.0/alpha traffic volumes don't need sampling
/// yet; this knob exists so it's adjustable later without a code change.
pub const SAMPLE_RATIO_ENV_VAR: &str = "COORDIN8_OTEL_SAMPLE_RATIO";

const DEFAULT_SAMPLE_RATIO: f64 = 1.0;

/// A [`SpanExporter`] that discards every span. Backs the tracer provider
/// when no OTLP endpoint is configured, so the exact same provider/layer
/// types are used whether or not export is enabled — see the module docs.
#[derive(Debug, Default)]
struct NoopExporter;

impl SpanExporter for NoopExporter {
    fn export(&self, _batch: Vec<SpanData>) -> impl Future<Output = OTelSdkResult> + Send {
        std::future::ready(Ok(()))
    }
}

/// Tracing configuration for one service, built from the environment —
/// matches every other Coordin8 knob (`COORDIN8_JWT_SECRET` et al.).
pub struct TracingConfig {
    service_name: String,
    endpoint: Option<String>,
    sample_ratio: f64,
}

impl TracingConfig {
    /// `service_name` identifies this process in exported traces (e.g.
    /// `"coordin8-registry"`) — passed in by the caller since it varies
    /// per split-mode subcommand, unlike the env-derived fields.
    pub fn from_env(service_name: impl Into<String>) -> Self {
        let endpoint = Self::parse_endpoint(std::env::var(ENDPOINT_ENV_VAR).ok().as_deref());
        let sample_ratio =
            Self::parse_sample_ratio(std::env::var(SAMPLE_RATIO_ENV_VAR).ok().as_deref());
        Self {
            service_name: service_name.into(),
            endpoint,
            sample_ratio,
        }
    }

    fn parse_endpoint(value: Option<&str>) -> Option<String> {
        value.filter(|s| !s.is_empty()).map(str::to_string)
    }

    fn parse_sample_ratio(value: Option<&str>) -> f64 {
        value
            .and_then(|v| v.parse::<f64>().ok())
            .map(|r| r.clamp(0.0, 1.0))
            .unwrap_or(DEFAULT_SAMPLE_RATIO)
    }

    /// Whether an OTLP endpoint is configured — i.e. whether anything
    /// actually gets exported. Propagation/span creation happen either way
    /// (Decision 3); this only reflects the export half.
    pub fn export_enabled(&self) -> bool {
        self.endpoint.is_some()
    }

    /// Builds the tracer provider — real (batched OTLP/gRPC export) if
    /// [`ENDPOINT_ENV_VAR`] is set, otherwise backed by [`NoopExporter`].
    /// Either way the result is a real [`SdkTracerProvider`], so
    /// `tracing_opentelemetry`'s bridge layer works identically in both
    /// cases; only whether spans leave the process differs.
    fn build_provider(&self) -> SdkTracerProvider {
        let resource = Resource::builder()
            .with_attribute(KeyValue::new("service.name", self.service_name.clone()))
            .build();
        let sampler = opentelemetry_sdk::trace::Sampler::TraceIdRatioBased(self.sample_ratio);

        let builder = SdkTracerProvider::builder()
            .with_resource(resource)
            .with_sampler(sampler);

        match &self.endpoint {
            Some(endpoint) => {
                match opentelemetry_otlp::SpanExporter::builder()
                    .with_tonic()
                    .with_endpoint(endpoint.clone())
                    .build()
                {
                    Ok(exporter) => builder.with_batch_exporter(exporter).build(),
                    Err(e) => {
                        tracing::warn!(
                            "failed to build OTLP exporter for {endpoint}: {e} — \
                             tracing spans will be created but not exported"
                        );
                        builder.with_batch_exporter(NoopExporter).build()
                    }
                }
            }
            None => builder.with_batch_exporter(NoopExporter).build(),
        }
    }

    /// Builds the `tracing-subscriber` layer bridging `tracing` spans into
    /// OpenTelemetry — pass to `Registry::with` alongside the logging
    /// layer from [`crate::LoggingConfig`]. Call once, at process startup.
    pub fn layer<S>(&self) -> impl tracing_subscriber::Layer<S> + Send + Sync
    where
        S: tracing::Subscriber
            + for<'span> tracing_subscriber::registry::LookupSpan<'span>
            + Send
            + Sync,
    {
        // The default global propagator is a no-op — without installing
        // the W3C tracecontext one, extract()/inject() below would never
        // read or write a `traceparent` header at all.
        global::set_text_map_propagator(TraceContextPropagator::new());

        let provider = self.build_provider();
        let tracer = provider.tracer(self.service_name.clone());
        // Leak the provider so its background batch-export task keeps
        // running for the process lifetime — every service here runs
        // until killed, none has a graceful-shutdown path today (matching
        // Open Question territory, not a regression this phase
        // introduces).
        Box::leak(Box::new(provider));
        tracing_opentelemetry::layer().with_tracer(tracer)
    }
}

/// Extracts a W3C `traceparent` (if present) from `headers` and returns the
/// resulting parent [`opentelemetry::Context`] — an empty context if none
/// was present, meaning the request becomes a new trace root rather than a
/// child of anything.
fn extract_parent_context(headers: &http::HeaderMap) -> opentelemetry::Context {
    global::get_text_map_propagator(|propagator| propagator.extract(&HeaderExtractor(headers)))
}

fn make_span(req: &http::Request<tonic::body::BoxBody>) -> tracing::Span {
    let parent_cx = extract_parent_context(req.headers());
    let span = tracing::info_span!(
        "grpc_request",
        otel.name = %req.uri().path(),
        rpc.system = "grpc",
        trace_id = tracing::field::Empty,
    );
    let _ = span.set_parent(parent_cx);
    // Record the resulting trace ID as a plain tracing field so it shows up
    // on every log line emitted within this span (Decision 4's log/trace
    // correlation) — tracing-opentelemetry bridges spans *into* OTel for
    // export, it doesn't inject IDs back into the log formatter on its own,
    // so this has to be done explicitly.
    let trace_id = span.context().span().span_context().trace_id();
    span.record("trace_id", tracing::field::display(trace_id));
    span
}

/// Builds the server-side layer that creates one span per gRPC call,
/// parented to whatever `traceparent` the caller sent (Decision 2). Apply
/// via `Server::builder().layer(coordin8_observability::server_layer())`
/// *before* `.add_service(...)` so it wraps every service mounted on that
/// server. Runs unconditionally, independent of whether OTLP export is
/// configured — see the module docs.
pub type ServerTraceLayer = tower_http::trace::TraceLayer<
    tower_http::classify::SharedClassifier<tower_http::classify::GrpcErrorsAsFailures>,
    fn(&http::Request<tonic::body::BoxBody>) -> tracing::Span,
>;

pub fn server_layer() -> ServerTraceLayer {
    tower_http::trace::TraceLayer::new_for_grpc()
        .make_span_with(make_span as fn(&http::Request<tonic::body::BoxBody>) -> tracing::Span)
}

/// Injects the currently active span's trace context as a `traceparent`
/// metadata entry on every outgoing call — the client half of Decision 2's
/// propagation. A plain `tonic::service::Interceptor`, the same shape as
/// `coordin8-auth`'s `ClientAuthInterceptor`: metadata mutation only, no
/// future-wrapping needed since injection only needs "whatever span is
/// active right now," not a span of its own.
#[derive(Clone, Default)]
pub struct ClientTraceInterceptor;

impl tonic::service::Interceptor for ClientTraceInterceptor {
    fn call(&mut self, mut req: tonic::Request<()>) -> Result<tonic::Request<()>, tonic::Status> {
        let cx = tracing::Span::current().context();
        global::get_text_map_propagator(|propagator| {
            propagator.inject_context(&cx, &mut MetadataInjector(req.metadata_mut()));
        });
        Ok(req)
    }
}

/// The concrete transport every internal Djinn-to-Djinn caller ends up
/// with: Decision 8's auth wrapping (`coordin8_auth::AuthedChannel`) with
/// trace-context injection layered on top. Every internal call site
/// already calls `coordin8_auth::wrap_channel` (self-registration,
/// `RemoteCapabilityResolver`, TxnMgr's participant voting) — swapping
/// those to [`wrap_traced_channel`] adds propagation for free, on the same
/// channel, without duplicating the auth wrapping.
pub type TracedAuthedChannel = tonic::service::interceptor::InterceptedService<
    coordin8_auth::AuthedChannel,
    ClientTraceInterceptor,
>;

/// Wraps `channel` with both Decision 8's client auth strategy and trace
/// propagation. Drop-in replacement for `coordin8_auth::wrap_channel` at
/// every internal Djinn-to-Djinn call site.
pub fn wrap_traced_channel(
    channel: tonic::transport::Channel,
    client_auth: &coordin8_auth::ClientAuthConfig,
) -> TracedAuthedChannel {
    let authed = coordin8_auth::wrap_channel(channel, client_auth);
    tonic::service::interceptor::InterceptedService::new(authed, ClientTraceInterceptor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_disabled_without_endpoint() {
        let cfg = TracingConfig {
            service_name: "test".into(),
            endpoint: None,
            sample_ratio: DEFAULT_SAMPLE_RATIO,
        };
        assert!(!cfg.export_enabled());
    }

    #[test]
    fn export_enabled_with_endpoint() {
        let cfg = TracingConfig {
            service_name: "test".into(),
            endpoint: Some("http://localhost:4317".into()),
            sample_ratio: DEFAULT_SAMPLE_RATIO,
        };
        assert!(cfg.export_enabled());
    }

    #[test]
    fn sample_ratio_clamps_to_valid_range() {
        assert_eq!(TracingConfig::parse_sample_ratio(Some("5.0")), 1.0);
        assert_eq!(TracingConfig::parse_sample_ratio(Some("-1.0")), 0.0);
    }

    #[test]
    fn sample_ratio_defaults_when_unset_or_invalid() {
        assert_eq!(
            TracingConfig::parse_sample_ratio(None),
            DEFAULT_SAMPLE_RATIO
        );
        assert_eq!(
            TracingConfig::parse_sample_ratio(Some("not-a-number")),
            DEFAULT_SAMPLE_RATIO
        );
    }

    #[test]
    fn endpoint_treats_empty_string_as_unset() {
        assert_eq!(TracingConfig::parse_endpoint(Some("")), None);
        assert_eq!(TracingConfig::parse_endpoint(None), None);
        assert_eq!(
            TracingConfig::parse_endpoint(Some("http://localhost:4317")),
            Some("http://localhost:4317".to_string())
        );
    }

    /// End-to-end (in-process) proof that the client half of propagation
    /// actually works: given an active span parented to a known incoming
    /// trace, the client interceptor's outgoing metadata carries that same
    /// trace ID — the exact mechanism internal Djinn-to-Djinn calls rely on
    /// (Decision 2). Complements the live grpcurl verification (server
    /// extraction + log correlation) done manually against a real Djinn.
    #[test]
    fn client_interceptor_injects_the_active_spans_trace_id() {
        use tonic::service::Interceptor as _;
        use tracing_subscriber::layer::SubscriberExt;

        let cfg = TracingConfig {
            service_name: "test".into(),
            endpoint: None,
            sample_ratio: DEFAULT_SAMPLE_RATIO,
        };
        let subscriber = tracing_subscriber::registry().with(cfg.layer());

        tracing::subscriber::with_default(subscriber, || {
            global::set_text_map_propagator(TraceContextPropagator::new());

            let known_trace_id = "4bf92f3577b34da6a3ce929d0e0e4736";
            let mut headers = http::HeaderMap::new();
            headers.insert(
                "traceparent",
                format!("00-{known_trace_id}-00f067aa0ba902b7-01")
                    .parse()
                    .unwrap(),
            );
            let parent_cx = extract_parent_context(&headers);

            let span = tracing::info_span!("test_span");
            let _ = span.set_parent(parent_cx);
            let _guard = span.enter();

            let mut interceptor = ClientTraceInterceptor;
            let req = interceptor.call(tonic::Request::new(())).unwrap();
            let traceparent = req
                .metadata()
                .get("traceparent")
                .expect("interceptor should have injected traceparent")
                .to_str()
                .unwrap();
            assert!(
                traceparent.contains(known_trace_id),
                "expected trace id {known_trace_id} in injected header {traceparent}"
            );
        });
    }
}
