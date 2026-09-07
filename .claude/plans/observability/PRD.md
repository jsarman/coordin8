# Production-Grade Observability — PRD

> **Status: In progress — all 3 scoped phases implemented, not yet merged to main.** New initiative, not part of the (now fully closed) `.claude/plans/hardening-roadmap/`. Implementation lives on branch `worktree-observability-phase1` (worktree `.claude/worktrees/observability-phase1`): structured JSON logging (Phase 1), OpenTelemetry tracing (Phase 2), and Prometheus metrics (Phase 3) are all done and live-verified end-to-end against a real running Djinn. Phase 4 (SDK-side trace propagation) remains unscheduled future work, not blocking. No PR opened yet — update this line to COMPLETE with the PR link once merged.

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

6. **Metric taxonomy fixed up front, RED method.** `coordin8_grpc_requests_total{grpc_service,grpc_method,grpc_code}` (counter) and `coordin8_grpc_request_duration_seconds{grpc_service,grpc_method}` (histogram), recorded once per RPC. **Revised during Phase 3 implementation** from this Decision's original wording (`coordin8_<service>_grpc_requests_total{method,code}` — service embedded in the metric *name*): a single metric name with a `grpc_service` label is what actually shipped, since per-service metric names would require registering a new Prometheus metric family dynamically per encountered service at runtime — an anti-pattern Prometheus's own client libraries actively discourage — whereas a label needs no such dynamic registration and carries the identical information. Same taxonomy, standard practice. **Cardinality rule, stated explicitly:** no label ever carries a tuple ID, capability ID, lease ID, or any other unbounded-cardinality value — enforced in code, not just documented: a gRPC transport failure's free-text description is deliberately collapsed to a fixed `"transport_error"` label rather than embedded verbatim. Any future metric addition must justify its label set against this rule.

7. **One new shared crate: `coordin8-observability`.** Mirrors the existing `coordin8-lease`/`coordin8-auth` pattern — a library every service embeds, not a service itself. Provides `LoggingConfig::from_env()` (Decision 1), `TracingConfig::from_env()` + server/client tonic interceptors (Decisions 2-4), and `MetricsConfig::from_env()` + the `/metrics` HTTP server (Decision 5-6) — metrics recording isn't a separate interceptor; it extends the *same* `server_layer()` `TraceLayer` Phase 2 already applies to every service, via its `on_response`/`on_failure` hooks. Wired into every service's bundled `run_all()` and every split-mode `run_*_on_listener`, the same integration points `coordin8-auth` already uses in `coordin8-djinn/src/services.rs`.

8. **Sampling defaults to always-on (ratio 1.0), but is a config knob.** Pre-1.0/alpha traffic volumes don't need sampling yet; `COORDIN8_OTEL_SAMPLE_RATIO` exists so it's adjustable later without a code change. Adaptive/tail-based sampling is out of scope (see Non-Goals).

9. **Scope v1 to the Djinn (Rust core) only — SDKs don't originate or propagate trace context yet.** A client-originated trace would need Go/Java/Node SDK changes (inject `traceparent` on outbound calls, same shape as JWT's Phase 2-4) — deferred as real future work, not blocking, since server-side fan-out is still fully visible without a client span (the trace just starts at the first Djinn hop instead of the caller). Matches the JWT PRD's own phase sequencing (Rust core first, proven, before touching three more languages).

## Plan

### Phase 1 — Structured logging ✅ DONE

New `coordin8-observability` crate: `LoggingConfig::from_env()` reads `COORDIN8_LOG_FORMAT` (default `pretty`) and builds the right `tracing-subscriber` layer (existing plaintext formatter, or a JSON one). Every service's entry point (bundled `run_all()`, every split-mode subcommand in `coordin8-djinn`) calls this once at startup instead of constructing its subscriber ad hoc. `RUST_LOG` behavior unchanged. Docker Compose examples gained a documented (not default-on) `COORDIN8_LOG_FORMAT=json` the same way `grpc-security`'s auth overlay works — opt-in, not forced on existing demos. Live-verified both output modes against the real binary, byte-identical pretty output to before.

### Phase 2 — OpenTelemetry tracing ✅ DONE

`TracingConfig::from_env()` (`COORDIN8_OTEL_ENDPOINT`, `COORDIN8_OTEL_SAMPLE_RATIO`, service name derived from the binary/subcommand). OTLP exporter via `opentelemetry-otlp`. Server-side tonic interceptor extracts an incoming `traceparent` (or starts a new root span if absent); client-side interceptor injects the current span's context into outgoing metadata — wired into the same call sites `coordin8-auth`'s `ClientAuthConfig` already covers (self-registration, `RemoteCapabilityResolver`, TxnMgr's participant voting). `tracing-opentelemetry` bridges spans into the JSON log layer from Phase 1, adding `trace_id`/`span_id` fields (Decision 4) — this needed doing explicitly via `with_span_events(FmtSpan::CLOSE)` plus recording the trace ID as a span field, since `tracing-opentelemetry` bridges spans *into* OTel for export but doesn't inject IDs back into log output on its own. Live-verified against a real Djinn via grpcurl: a hand-crafted `traceparent`'s trace ID appeared verbatim in the resulting JSON log line; a call with no header got a fresh root trace; a focused in-process test proved client-side injection too.

### Phase 3 — Prometheus metrics ✅ DONE

`MetricsConfig::from_env()` (`COORDIN8_METRICS_PORT`, unset = disabled). The RED-method metrics from Decision 6 are recorded by extending `server_layer()`'s `on_response`/`on_failure` hooks (not a separate interceptor); a small `hyper`-based HTTP server (only bound when the port is set) exposes `/metrics` in Prometheus text format, spawned as a background task from `coordin8-djinn`'s `main()` alongside the existing `coordin8_observability::init()` call. Process-level metrics (CPU, RSS, fd/thread counts) come from `prometheus::process_collector::ProcessCollector`, Linux-only (`/proc`-based) — a `#[cfg(target_os = "linux")]`-gated no-op elsewhere, since every real deployment target (Docker) is Linux anyway. Live-verified against a real Djinn: a mix of successful and deliberately-failing RPCs produced exactly the right `coordin8_grpc_requests_total` counts split by method and gRPC code (including the failure, correctly classified), with proper histogram buckets; confirmed the disabled case never opens the port at all.

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
