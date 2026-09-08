//! Structured logging setup — Phase 1 of
//! `.claude/plans/observability/PRD.md`.

use tracing_subscriber::Layer;

/// Env var selecting the log output format. Unset, or anything other than
/// `"json"` (case-insensitive), means [`LogFormat::Pretty`] — the existing
/// human-readable output, byte-for-byte unchanged from before this crate
/// existed. `RUST_LOG` keeps controlling level/per-crate filtering exactly
/// as before; this only changes the output encoding.
pub const LOG_FORMAT_ENV_VAR: &str = "COORDIN8_LOG_FORMAT";

/// How log lines are encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    /// Human-readable text — the default. Right for local dev, a
    /// terminal, `docker compose logs`.
    Pretty,
    /// One JSON object per line to stdout — the standard cloud-native
    /// pattern a log-shipping agent (Splunk UF, Datadog Agent, Vector,
    /// Fluentd, ...) tails directly. No service pushes to a specific
    /// vendor's API; this only controls the encoding on stdout.
    Json,
}

impl LogFormat {
    fn from_opt(value: Option<&str>) -> Self {
        match value {
            Some(v) if v.eq_ignore_ascii_case("json") => LogFormat::Json,
            _ => LogFormat::Pretty,
        }
    }

    fn from_env() -> Self {
        Self::from_opt(std::env::var(LOG_FORMAT_ENV_VAR).ok().as_deref())
    }
}

/// Logging configuration for one service. The only way to build one in v1
/// is [`LoggingConfig::from_env`] — matches every other Coordin8 knob
/// (`COORDIN8_JWT_SECRET` et al.): config comes from the environment, not
/// a builder API a caller assembles by hand.
pub struct LoggingConfig {
    format: LogFormat,
}

impl LoggingConfig {
    pub fn from_env() -> Self {
        Self {
            format: LogFormat::from_env(),
        }
    }

    pub fn format(&self) -> LogFormat {
        self.format
    }

    /// Builds the log-formatting layer — pass to `Registry::with` alongside
    /// the tracing layer from [`crate::TracingConfig`]. Boxed because
    /// `Pretty`/`Json` produce different concrete `fmt::Layer` types and
    /// this needs to return one or the other from the same function.
    ///
    /// Does *not* log a line on every span close (e.g. Phase 2's per-RPC
    /// `grpc_request` span, mounted on every service): with a self-
    /// registering split-mode service renewing its lease every `ttl/3`
    /// seconds, plus `ServiceDiscovery` cache refreshes and health checks,
    /// that produced one line per RPC completion on every service, all the
    /// time — pure noise, not signal. What it would have carried is
    /// available elsewhere without the noise: `trace_id` is already on
    /// every log line emitted *within* the span (via `span.record` +
    /// `otel::make_span` — a separate mechanism from span-close events),
    /// and duration is captured by both the exported OTel span and Phase
    /// 3's `coordin8_grpc_requests_total`/`..._duration_seconds` Prometheus
    /// histograms.
    pub fn fmt_layer<S>(&self) -> Box<dyn Layer<S> + Send + Sync>
    where
        S: tracing::Subscriber + for<'span> tracing_subscriber::registry::LookupSpan<'span>,
    {
        match self.format {
            LogFormat::Pretty => Box::new(tracing_subscriber::fmt::layer()),
            LogFormat::Json => Box::new(tracing_subscriber::fmt::layer().json()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_pretty_when_unset() {
        assert_eq!(LogFormat::from_opt(None), LogFormat::Pretty);
    }

    #[test]
    fn defaults_to_pretty_for_unrecognized_value() {
        assert_eq!(LogFormat::from_opt(Some("syslog")), LogFormat::Pretty);
        assert_eq!(LogFormat::from_opt(Some("")), LogFormat::Pretty);
    }

    #[test]
    fn json_when_set() {
        assert_eq!(LogFormat::from_opt(Some("json")), LogFormat::Json);
    }

    #[test]
    fn json_is_case_insensitive() {
        assert_eq!(LogFormat::from_opt(Some("JSON")), LogFormat::Json);
        assert_eq!(LogFormat::from_opt(Some("Json")), LogFormat::Json);
    }
}
