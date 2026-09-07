//! Shared observability setup for every Coordin8 gRPC service.
//!
//! `.claude/plans/observability/PRD.md` — three sequenced pillars:
//! structured logging (Phase 1, [`logging`]), OpenTelemetry tracing
//! (Phase 2, [`otel`]), Prometheus metrics (Phase 3, [`metrics`]).
//!
//! `coordin8-djinn`'s single binary entry point (which covers both the
//! bundled monolith and every split-mode subcommand — there's exactly one
//! `main()`) calls [`init`] once at startup instead of constructing a
//! `tracing-subscriber` ad hoc, and [`MetricsConfig::spawn`] to start the
//! `/metrics` endpoint if configured.

mod logging;
mod metrics;
mod otel;
mod propagation;

pub use logging::{LogFormat, LoggingConfig, LOG_FORMAT_ENV_VAR};
pub use metrics::{MetricsConfig, PORT_ENV_VAR as METRICS_PORT_ENV_VAR};
pub use otel::{
    server_layer, wrap_traced_channel, ClientTraceInterceptor, ServerTraceLayer,
    TracedAuthedChannel, TracingConfig, ENDPOINT_ENV_VAR, SAMPLE_RATIO_ENV_VAR,
};

use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

/// Installs the global `tracing` subscriber for this process: structured
/// logging (Phase 1) plus OpenTelemetry span creation/propagation
/// (Phase 2, running unconditionally per Decision 3 even when OTLP export
/// isn't configured). Call exactly once, at process startup, before any
/// other `tracing` call. `service_name` identifies this process in
/// exported traces (e.g. `"coordin8-registry"`).
///
/// Does *not* start the `/metrics` server — that's
/// [`MetricsConfig::spawn`], called separately once a service has an
/// async runtime running (this function is sync, callable before
/// `#[tokio::main]`'s runtime exists).
pub fn init(service_name: impl Into<String>) {
    let filter = EnvFilter::from_default_env()
        .add_directive("coordin8=info".parse().expect("static directive is valid"));

    let logging = LoggingConfig::from_env();
    let tracing_cfg = TracingConfig::from_env(service_name);

    tracing_subscriber::registry()
        .with(filter)
        .with(logging.fmt_layer())
        .with(tracing_cfg.layer())
        .init();
}
