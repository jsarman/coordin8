# Boot-Order Independence + Health Signaling — Design Draft

> **Status: Design decided 2026-09-05, implementation starting.** Detailed design for roadmap item 1. Findings below are from live code reading + an empirical test in a worktree on 2026-09-05 (build, run split-mode services out of order, observe). See `.claude/plans/space-race-txn-failsafe/session-1-complete.md` for the session that led here, and `decisions.md` for the two open questions below, now resolved: background-resolve + fast-fail (Option B), and Check-only for the health RPC surface.

## Problem Statement (what's actually true today)

**Boot-order independence in the "won't crash, self-heals" sense already exists.** Verified two ways:

1. **Code:** `coordin8-bootstrap`'s `retry_forever` (100ms→5s exponential backoff, no cap on duration) backs `discover_lease_mgr`, `discover_txn_mgr`, `RemoteLeasing::connect`, and `RemoteCapabilityResolver::connect`. Every split-mode service that needs Registry/LeaseMgr uses one of these.
2. **Live test:** started `djinn event` with `COORDIN8_REGISTRY` pointing at nothing. It logged `retrying: transport error` with growing backoff for 31s, then the instant `djinn registry` + `djinn lease` came up, it logged `discovered LeaseMgr`, `EventMgr (split): listening...`, `Djinn event ready`, `self-registered in Registry` — all without crashing or restarting.

Per-service dependency picture (confirmed by reading `djinn/crates/coordin8-djinn/src/services.rs`):

| Service | Blocking dependency at boot | Pattern |
|---|---|---|
| Registry | none — own private in-process `LeaseManager` for entry TTLs | serves immediately |
| LeaseMgr | none (Registry only needed for optional self-registration) | serves immediately; self-registration retries **concurrently** via `tokio::select!` (`services.rs:514-517`) — this is the model to copy |
| EventMgr | Registry + LeaseMgr, via `RemoteLeasing::connect` | **blocks before serving** (`services.rs:558`) |
| Space | Registry + LeaseMgr, via `RemoteLeasing::connect` + 2 more `discover_lease_mgr` calls for watch streams. TxnMgr correctly deferred/lazy already (`services.rs:687-690`) | **blocks before serving**, 3 sequential discoveries (`services.rs:684,709,728`) |
| TxnMgr | Registry + LeaseMgr, via `RemoteLeasing::connect` + 1 more `discover_lease_mgr` | **blocks before serving** (`services.rs:844,850`) |
| Proxy | Registry, via `RemoteCapabilityResolver::connect` | **blocks before serving** (`services.rs:964`), self-registration is concurrent like LeaseMgr's |

**So the real gap is narrower than "boot order":** four services delay their entire gRPC serve loop behind a retry-forever call, instead of serving immediately and letting only the dependency-specific work wait (LeaseMgr's own pattern already shows how). And separately:

**There is no real health signal at all.** Confirmed empirically: `TcpListener::bind()` happens before the blocking discovery call, so the TCP port is already accepting connections during the entire wait — a bare TCP probe (what CLAUDE.md documents: `timeout 1 bash -c '</dev/tcp/localhost/9001'`) reports "up" the whole time. That's a false positive, not a false negative: nothing gets restart-looped, but nothing tells a caller (or an operator, or an orchestrator that actually wants to gate traffic) that the service can't yet do real work. A live gRPC call made during the wait would just hang/timeout with no informative status.

## Goals

1. All split-mode services start serving their gRPC port immediately at boot, regardless of dependency state — matching LeaseMgr's existing pattern.
2. Requests that need an unresolved dependency fail fast with a clear, typed status (not a hang, not a crash) while it's still resolving.
3. A real health-check surface exists that can distinguish: fully operational vs. alive-but-waiting-on-dependency-X vs. actually unreachable (the last one already works today — no process, no TCP accept, handled by dropping/restart at the orchestrator level).
4. Docker/orchestrator health checks for split-mode services key off that real signal, not a bare TCP probe — needed for roadmap item 2 (one container per service) to have a healthcheck that means anything.

## Non-Goals

- Not touching the retry/backoff logic itself — `retry_forever` and its callers are correct and stay as-is.
- Not building the AWS/SNS broadcast-externalization work from `phase3-cloud-topology/resilience-plan.md` — this is Docker/Compose-scoped, ahead of any cloud topology.
- Not changing Space's already-correct lazy TxnMgr handling — it's the existing precedent this design generalizes.

## Design Question: how does "serve immediately" interact with construction?

Every manager (`EventManager`, `SpaceManager`, `TxnManager`) currently takes an already-resolved `Arc<dyn Leasing>` at construction time — which is *why* the blocking `RemoteLeasing::connect().await?` happens before the manager (and therefore the server) can exist at all. Two ways to fix this, real tradeoffs, no clear-cut winner — flagging for review rather than picking solo:

### Option A — make `RemoteLeasing::connect` non-blocking; resolve lazily on first real call

Change `RemoteLeasing` to construct immediately holding an unresolved/pending client, doing the `retry_forever` discovery lazily inside `call_with_retry` on first actual use — the same place it *already* re-resolves after a transport failure mid-run (`services.rs` / `coordin8-bootstrap/src/lib.rs:407-423`). Managers construct immediately, the server starts immediately, and a request that lands before LeaseMgr is discovered just experiences the wait as part of that request's latency (fails or hangs no differently than a slow LeaseMgr would today) rather than the whole process being unable to serve *anything*.

- **Pros:** Minimal new concepts — reuses the exact lazy-discovery mechanism Space's TxnMgr dependency already proves out. Symmetric: every dependency (LeaseMgr from Space/EventMgr/TxnMgr/Proxy, TxnMgr from Space) becomes "resolve on demand," no special cases.
- **Cons:** Changes `RemoteLeasing::connect`'s contract (today it's synchronously "either you have a working client or you waited for one" — callers may rely on that). A request arriving during the unresolved window still just hangs from the caller's perspective (same UX problem, just moved from "whole server down" to "this one call is slow") unless combined with a fast-fail check (see health signal below) — doesn't fully solve goal 2 on its own.

### Option B — background-resolve behind a swappable handle, fail fast until ready

Keep `RemoteLeasing::connect` blocking as today, but run it in a background task (mirroring LeaseMgr's `tokio::select!` self-registration pattern) that populates an `ArcSwapOption<RemoteLeasing>` (or similar) once discovery succeeds. Managers take a handle that's `None` until then. Server starts immediately with all services registered; any RPC handler that needs the dependency and finds it still `None` returns `tonic::Status::unavailable("waiting for dependency: LeaseMgr")` immediately instead of hanging.

- **Pros:** Directly satisfies goal 2 (explicit fast-fail, not a hang) without changing `RemoteLeasing`'s existing contract. Clean mapping to the health-check states below (`None` → NOT_SERVING).
- **Cons:** More boilerplate — every RPC handler on the four affected services needs an early "is my dependency ready" check. Two closely-related-but-distinct "waiting" mechanisms exist afterward (`RemoteLeasing`'s own retry-forever for *transport failures after* initial resolution, vs. this new pre-resolution gate).

**Decided: Option B** — see `decisions.md`.

## Health-Check Surface

Recommend adopting the standard **gRPC Health Checking Protocol** (`grpc.health.v1.Health`, `Check` + `Watch` RPCs) rather than inventing a custom one — `tonic-health` (the standard crate for this in the tonic ecosystem) provides a `health_reporter()` + `HealthServer` that plugs in alongside the existing services with `.set_serving::<T>()` / `.set_not_serving::<T>()` calls. This is what `grpc_health_probe` and most container orchestrators already know how to speak, so this also directly unblocks meaningful Docker healthchecks for roadmap item 2.

Mapping:
- **SERVING** — dependency resolved, fully operational (today's steady state, unchanged).
- **NOT_SERVING** — alive, listening, retrying its dependency — exactly "healthy but waiting," reported explicitly instead of accidentally implied by a TCP probe that can't tell the difference.
- **Genuinely down** — no process, nothing accepting connections at all — already handled correctly today (orchestrator's connection-refused path), not something this design needs to touch.

CLAUDE.md's "Docker" gotchas section documents the current bare-TCP healthcheck (`timeout 1 bash -c '</dev/tcp/localhost/9001'`) for the bundled monolith on :9001 — that's fine to leave as-is for bundled mode (LeaseMgr has no blocking dependency, so TCP-up genuinely means ready there). This design's health surface targets split-mode's other four services specifically, and becomes load-bearing once roadmap item 2 gives them their own containers/healthchecks.

## Suggested Sequencing

1. ~~Land the health-check surface first~~ — **done.** `tonic-health` wired into all six split-mode services, verified live with `grpcurl` against Registry/LeaseMgr/EventMgr (the three structurally distinct patterns). All report `SERVING` at the point they're about to accept requests — no behavior change yet, since none have been restructured to serve before their dependency resolves.
2. **Next:** restructure EventMgr/Space/TxnMgr/Proxy per Option B (background-resolve + fast-fail, see `decisions.md`), wiring `set_not_serving`/`set_serving` to the actual dependency-resolution state (currently always `Serving` since step 1 landed with no behavior change).
3. Update CLAUDE.md's Docker gotchas + the master `.claude/plans/PRD.md` Docker & Orchestration table once this ships.

## Open Questions

1. ~~Option A vs. B~~ — resolved, see `decisions.md`.
2. ~~Check vs. Check+Watch~~ — resolved, see `decisions.md`.
3. Per-service granularity: `tonic-health` reports status per registered gRPC service name (or empty string for "overall") — do we want one overall status per Djinn split-mode process, or does it matter that e.g. Space's ParticipantService and its main SpaceService could theoretically report differently? (Probably not distinct — a Space process is either ready or not as a whole.) Still open, low-stakes — will default to "overall" unless implementation surfaces a reason not to.
