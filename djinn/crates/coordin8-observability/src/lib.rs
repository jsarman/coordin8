//! Shared observability setup for every Coordin8 gRPC service.
//!
//! Phase 1 (structured logging) of `.claude/plans/observability/PRD.md`.
//! `coordin8-djinn`'s single binary entry point (which covers both the
//! bundled monolith and every split-mode subcommand — there's exactly one
//! `main()`) calls [`LoggingConfig::from_env`] + [`LoggingConfig::init`]
//! once at startup instead of constructing a `tracing-subscriber` ad hoc.

use tracing_subscriber::EnvFilter;

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

    /// Installs the global `tracing` subscriber. Call exactly once, at
    /// process startup, before any other `tracing` call.
    pub fn init(&self) {
        let filter = EnvFilter::from_default_env()
            .add_directive("coordin8=info".parse().expect("static directive is valid"));

        match self.format {
            LogFormat::Pretty => {
                tracing_subscriber::fmt().with_env_filter(filter).init();
            }
            LogFormat::Json => {
                tracing_subscriber::fmt()
                    .json()
                    .with_env_filter(filter)
                    .init();
            }
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
