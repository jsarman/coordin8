# Decisions — Hardening Roadmap

---

## Serve immediately via background-resolve + fast-fail (not lazy-resolve)

**Decision:** EventMgr/Space/TxnMgr/Proxy start their gRPC server immediately at boot. Dependency discovery (LeaseMgr via Registry, etc.) runs in a background task that populates a swappable handle once resolved. Any RPC needing that dependency before it's ready returns `tonic::Status::unavailable("waiting on dependency: <X>")` immediately rather than hanging.

**Why:** The alternative (make `RemoteLeasing::connect` lazy, resolve on first real call) reuses more existing machinery, but a request arriving early still just hangs — it moves the "whole server looks stuck" problem down to "this one call looks stuck," without producing an honest, typed signal. Fast-fail gives callers (and the health check below) something concrete to react to.

**Trade-off:** More boilerplate — every affected RPC handler needs an early readiness check instead of relying on construction-time guarantees.

---

## Health surface: gRPC Health Checking Protocol, Check only (Watch deferred, not precluded)

**Decision:** Adopt the standard `grpc.health.v1.Health` protocol via `tonic-health`, and only design/test against the `Check` (unary poll) RPC for now.

**Why:** `tonic-health`'s `HealthServer` implements both `Check` and `Watch` from the same `set_serving`/`set_not_serving` calls on the reporter — there's no wire-level cost to "choosing" Check only, so this isn't deferring real work, just deferring the *design and test investment* Watch would need to be actually useful: debouncing rapid state flaps before pushing them to watchers, and a streaming-client test to verify transitions deliver correctly. No consumer needs that today (`.claude/plans/PRD.md`'s Dashboard & Observability section is still "Not started"). `Check` alone is also already sufficient for both Docker's own `HEALTHCHECK` mechanism and Kubernetes' native `grpc` probe type (1.24+, calls `Check`) — Kubernetes is a real target deployment for some users, confirmed 2026-09-05, so this isn't Docker-only groundwork.

**If this becomes a problem:** When an actual push-based consumer shows up (dashboard, service mesh), add debouncing at the `set_serving`/`set_not_serving` call sites and a streaming test — the RPC itself needs no new code, it already works.
