# Production-Grade Observability — PRD

> **Status: Planning — not started.** New initiative, not part of the (now fully closed) `.claude/plans/hardening-roadmap/`. Explicit user direction: plan this properly before writing any code, same research-then-design pass as `distributed-leasing` and `grpc-security` got — this is cross-cutting across all five core Djinn services.

## Goal

Every core Djinn service (Registry, EventMgr, Space, Proxy, TransactionMgr) can, when configured to, emit structured JSON logs, OpenTelemetry distributed traces, and Prometheus metrics — the three pillars needed to actually operate Coordin8 in a real deployment (Splunk/Datadog log ingestion, an OTel collector, a Prometheus scrape target) instead of `RUST_LOG`-controlled plaintext to a terminal.

## Motivation

Coordin8's services today log via `tracing`/`tracing-subscriber` with plaintext output and per-crate `RUST_LOG` filtering — fine for local dev, useless for a production deployment where logs need to be machine-parseable, traces need to cross service boundaries (Registry → Proxy → Space, TxnMgr fanning out to participants), and metrics need to feed an alerting pipeline. This is the natural next step after `grpc-security`: that PRD made the services safe to run in a real deployment; this one makes them *operable* in one.

## Decisions

1. **Structured logging: JSON lines to stdout, not a network push.** Each service emits one JSON object per log line via `tracing-subscriber`'s JSON formatting layer — the standard cloud-native pattern where a log-shipping agent (Splunk UF, Datadog Agent, Vector, Fluentd — whatever the deployment already runs) tails the container's stdout. No service takes on HTTP-push/HEC/syslog client code for a specific vendor. Controlled by a new `COORDIN8_LOG_FORMAT` env var (`json` | `pretty`, default `pretty` — matches the project's established default-to-simplest-local-behavior pattern from `COORDIN8_JWT_SECRET` et al.). `RUST_LOG` keeps controlling level/per-crate filtering exactly as today — this only changes the output encoding.

2. **Trace-context propagation is the load-bearing piece — not optional, not deferred.** Coordin8 is inherently multi-hop (Registry/Proxy/Space are independent peers; TxnMgr fans out to participants; Decision 8 of `grpc-security` already added internal Djinn-to-Djinn calls for self-registration and capability resolution). A trace that stops at the first service's boundary defeats the purpose. Every internal call propagates a [W3C Trace Context](https://www.w3.org/TR/trace-context/) `traceparent` header via gRPC metadata, mirroring exactly how `coordin8-auth`'s `AuthConfig`/`ClientAuthConfig` split already threads the bearer token through the same call sites (server extracts + starts a child span; client injects the current span's context into outgoing metadata).

3. **Context propagation runs unconditionally; *exporting* is what's opt-in.** Unlike JWT auth (fully no-op when unconfigured), span creation and `traceparent` propagation happen on every request regardless of config — this is what makes traces exist at all, and the cost is negligible (one header, one span object). What's actually gated behind `COORDIN8_OTEL_ENDPOINT` (unset = disabled) is whether anything gets *exported* to an OTLP collector. This means an operator can point an OTel collector at any subset of services later without needing every hop specially reconfigured first — the plumbing is already there, just not shipping anywhere until told to.

4. **Log/trace correlation.** Once tracing-context propagation (Decision 2) is in place, `tracing-opentelemetry` bridges the active span into every log line's JSON output as `trace_id`/`span_id` fields — sequenced as part of Phase 2, since Phase 1's JSON logs alone have no trace context yet to inject. This is what lets someone jump from a Splunk/Datadog log line straight to the matching trace, rather than treating logging and tracing as two disconnected pillars.

5. **Prometheus: native pull-based `/metrics` endpoint, not routed through an OTel Collector.** Traces go out via OTLP push (Decision 2/3); metrics stay a separate, simpler pull-based HTTP endpoint each service exposes directly — the standard Prometheus pattern, and avoids forcing every metric through a collector pipeline OTel's metrics API doesn't need to gate. Controlled by `COORDIN8_METRICS_PORT` (unset = no HTTP server bound at all — genuinely zero overhead, not just an unused code path).

6. **Metric taxonomy fixed up front, RED method.** `coordin8_<service>_grpc_requests_total{method,code}` (counter) and `coordin8_<service>_grpc_request_duration_seconds{method}` (histogram), recorded once per RPC by a shared interceptor — not five services independently improvising five metric shapes. **Cardinality rule, stated explicitly:** no label ever carries a tuple ID, capability ID, lease ID, or any other unbounded-cardinality value. Any future metric addition must justify its label set against this rule.

7. **One new shared crate: `coordin8-observability`.** Mirrors the existing `coordin8-lease`/`coordin8-auth` pattern — a library every service embeds, not a service itself. Provides `LoggingConfig::from_env()` (Decision 1), `TracingConfig::from_env()` + server/client tonic interceptors (Decisions 2-4), and `MetricsConfig::from_env()` + a metrics-recording interceptor + the `/metrics` HTTP server (Decision 5-6). Wired into every service's bundled `run_all()` and every split-mode `run_*_on_listener`, the same integration points `coordin8-auth` already uses in `coordin8-djinn/src/services.rs`.

8. **Sampling defaults to always-on (ratio 1.0), but is a config knob.** Pre-1.0/alpha traffic volumes don't need sampling yet; `COORDIN8_OTEL_SAMPLE_RATIO` exists so it's adjustable later without a code change. Adaptive/tail-based sampling is out of scope (see Non-Goals).

9. **Scope v1 to the Djinn (Rust core) only — SDKs don't originate or propagate trace context yet.** A client-originated trace would need Go/Java/Node SDK changes (inject `traceparent` on outbound calls, same shape as JWT's Phase 2-4) — deferred as real future work, not blocking, since server-side fan-out is still fully visible without a client span (the trace just starts at the first Djinn hop instead of the caller). Matches the JWT PRD's own phase sequencing (Rust core first, proven, before touching three more languages).

## Plan

### Phase 1 — Structured logging

New `coordin8-observability` crate: `LoggingConfig::from_env()` reads `COORDIN8_LOG_FORMAT` (default `pretty`) and builds the right `tracing-subscriber` layer (existing plaintext formatter, or a JSON one). Every service's entry point (bundled `run_all()`, every split-mode subcommand in `coordin8-djinn`) calls this once at startup instead of constructing its subscriber ad hoc. `RUST_LOG` behavior unchanged. Docker Compose examples gain a documented (not default-on) `COORDIN8_LOG_FORMAT=json` the same way `grpc-security`'s auth overlay works — opt-in, not forced on existing demos.

### Phase 2 — OpenTelemetry tracing

`TracingConfig::from_env()` (`COORDIN8_OTEL_ENDPOINT`, `COORDIN8_OTEL_SAMPLE_RATIO`, service name derived from the binary/subcommand). OTLP exporter via `opentelemetry-otlp`. Server-side tonic interceptor extracts an incoming `traceparent` (or starts a new root span if absent); client-side interceptor injects the current span's context into outgoing metadata — wired into the same call sites `coordin8-auth`'s `ClientAuthConfig` already covers (self-registration, `RemoteCapabilityResolver`, TxnMgr's participant voting). `tracing-opentelemetry` bridges spans into the JSON log layer from Phase 1, adding `trace_id`/`span_id` fields (Decision 4).

### Phase 3 — Prometheus metrics

`MetricsConfig::from_env()` (`COORDIN8_METRICS_PORT`, unset = disabled). A tonic interceptor records the RED-method metrics from Decision 6 on every RPC; a small HTTP server (only bound when the port is set) exposes `/metrics` in Prometheus text format. Wired identically across every service.

### Phase 4 (future, not this PRD) — SDK-side trace propagation

If/when a real need for client-originated traces emerges: Go/Java/Node SDKs inject `traceparent` on outbound calls, mirroring JWT's Phase 2-4 shape exactly. Not scoped or scheduled here — noted so it isn't forgotten.

## Non-Goals (for this PRD — not forever, just not now)

- **SDK-side trace propagation for v1** — see Decision 9 / Phase 4.
- **CLI instrumentation** — the CLI is a short-lived one-shot process; tracing a single invocation is low value compared to the services it talks to.
- **Adaptive/tail-based sampling** — a fixed, configurable ratio (Decision 8) is enough for pre-1.0 traffic volumes.
- **Bundled dashboards, alerting rules, or a Grafana starter kit** — valuable, but a separate deliverable from making the services *emit* the right data in the first place.
- **Log-shipping agent configuration** — which agent tails stdout and where it forwards to is an operator/deployment concern, not something Coordin8's services configure themselves (Decision 1).
- **Unifying Prometheus behind an OTel Collector** — kept as two separate exporters per Decision 5, not forced into one pipeline.

## Open Questions (to resolve during implementation, not blocking this plan)

1. Exact Rust crate choices — `opentelemetry`/`opentelemetry-otlp`/`opentelemetry-sdk` versions, and whether metrics use the `metrics`+`metrics-exporter-prometheus` crates or the `prometheus` crate directly. Not a blocking decision; pick during Phase 1/3 implementation based on what's actively maintained at the time.
2. Whether `COORDIN8_METRICS_PORT` should have a documented default when auth/production mode is otherwise "on" (e.g. always `9090`-style convention) versus staying fully unset-by-default forever. Leaning toward "always opt-in, no implied default" to match Decision 5's zero-overhead framing, but worth a real decision at Phase 3.
3. Service-name convention for OTel resource attributes (e.g. `coordin8-registry` vs `coordin8.registry`) — pick whatever the OTel semantic conventions actually recommend at implementation time.
