# Distributed Leasing (Jini Landlord Pattern) — PRD

> **Status: Phases 1, 2, and most of 3/4 done and live-verified (2026-09-06).** Supersedes and reverses part of `.claude/plans/registry-bootstrap/` Phase 1b. Java/Node SDK work (registry-bootstrap Phases 3/4) stays paused until Node/Java get their own pass under this model. See `session-1-complete.md` in this folder for the full writeup.

## Goal

Remove `LeaseMgr` as a centralized network service. Each service that grants leased resources — Registry, Space, EventMgr, TransactionMgr — becomes its own **Landlord**: it grants, renews, cancels, persists, and reaps its own leases in-process, with no network hop and no dependency on any other service for its own bookkeeping. This is what Apache River (Jini's reference implementation) actually does — `com.sun.jini.landlord` is a library every grantor embeds, never a daemon anything calls over the wire. Coordin8 promoted leasing to a shared Layer-1 "bedrock" service instead; this reverses that.

## Motivation

Two independent research passes converged on the same conclusion:

1. This session's own experience: Registry-Only Bootstrap's Phase 1b (`.claude/plans/registry-bootstrap/PRD.md`) spent a full session fixing a self-referential bootstrap cycle — Registry can't discover LeaseMgr through a Registry lookup, since it *is* the lookup service — that **only exists because of the centralization decision**. The fix (`COORDIN8_LEASE` direct-dial, `RemoteLeasing::connect_direct`) works, but it's a symptom fix for a design that shouldn't have created the problem in the first place. Worth saying plainly: Registry's pre-fix behavior (a private, disconnected, in-process `LeaseManager`) was actually closer to the *correct* Jini-faithful design than what we "fixed" it to.
2. A second agent's deep-dive comparison (preserved in full at `jini-vs-coordin8-leasing-research.md` in this folder) reaches the same root diagnosis independently: "In Jini, a lease is an object held by the client, and every service is its own lease grantor. In Coordin8, a lease is a string ID held by the client, and one central service grants all leases for everyone. Almost every discrepancy falls out of that single inversion."

Concrete costs of the centralized model, beyond the bootstrap-cycle pain already felt:

- **SPOF + bottleneck.** Every tuple write, registration, subscription, and transaction begin currently costs a network round-trip to one shared LeaseMgr before it can proceed. Jini's `JavaSpace.write` granted its lease locally.
- **Scaling cliff.** A Space with 10k tuples on 30s TTLs needs ~660 renewal RPCs/sec against one shared service. Decentralized, that load is local per-service and trivial.
- **Failure blast radius.** LeaseMgr degrading takes down Registry, Space, EventMgr, and TxnMgr simultaneously — one dependency failure cascading system-wide.
- **Doesn't match the project's own stated lineage.** Coordin8 explicitly models itself on Jini/JavaSpaces; this is the one place the implementation diverged from the reference design without that being a deliberate, examined choice.

It's alpha, pre-1.0, no external users. Correcting the model now costs an architecture change confined to the Rust core plus one already-shipped SDK slice; correcting it later costs a breaking migration across three SDKs and real deployments.

## What changes

### Core primitive stays, its deployment shape doesn't

`coordin8_lease::LeaseManager` (store + `LeaseConfig` policy + reaper) is already structured as an embeddable library, not inherently a service — event/space/txn's own test demos already construct one directly (`coordin8-lease` is a plain crate dependency of `coordin8-event`, `coordin8-space`, `coordin8-txn` today, used exactly this way in their `tests/demo.rs`). The change is that **every service that needs leased resources gets its own instance**, in-process, instead of all four dialing one shared one over gRPC.

### Per-service Landlord

Registry, Space, EventMgr, and TransactionMgr each:
- Construct their own `LeaseManager` (own `LeaseStore` — in-memory or `DynamoLeaseStore::with_table(client, "<service>_leases")`, already supports a per-instance table name, no store redesign needed).
- Run their own reaper task, broadcasting expiry on their own local channel — no more shared `expiry_tx` fanned out to four subscribers filtering by `resource_id.starts_with("space:")`/`"registry:"`/`"txn:"`. Each service's reaper only ever contains its own resources, so **the string-prefix convention disappears entirely** — a clean side effect of decentralizing, not a separate fix.
- Mount the existing `LeaseService` gRPC implementation (`coordin8_lease::service::LeaseServiceImpl`, reused as-is) onto their **own** tonic server, alongside their primary service (e.g. Registry's server serves `RegistryService` + `LeaseService` both on :9002). A lease holder renews by calling the same service it already has a connection to — exactly Jini's model of the grantor's own remote object implementing `Landlord`.
- Get their own `LeaseConfig{max_ttl, preferred_ttl}`, tuned per resource type instead of one global policy (directly resolves the research doc's point that a 2PC transaction and a service registration shouldn't share a 3600s default cap).

Space currently has two lease namespaces (`space:` tuples, `space-watch:` watches) under one shared LeaseMgr — decide during implementation whether that becomes two `LeaseManager` instances or one with an internal type tag; no longer needs the global-namespace string-prefix trick either way.

### Self-describing leases

Add a `grantor_endpoint` (host:port) field to the `Lease` proto message (and ideally `grantor_uuid`, mirroring Jini's `LandlordLease(cookie, landlord, landlordUuid, expiration)`). With no single well-known LeaseMgr address, a holder needs to know *where* to renew — this is the mechanism. Bundled as one breaking wire change alongside:

- **Sentinel swap:** `FOREVER` becomes `u64::MAX` (was `0`), `ANY` becomes `0`→"use server's preferred TTL" (was `u64::MAX`... actually currently `ANY` doesn't have a clean sentinel at all per the research doc — audit during implementation). Current `0` colliding with proto3's default-unset value means a client that forgets to set `ttl_seconds` accidentally requests an eternal, unreapable lease. Swapping so the accidental-default value maps to the *safe* outcome.
- **Batch renewal:** add `RenewAll(repeated {lease_id, ttl_seconds}) returns (repeated RenewResult)` to the (now per-service-mounted) `LeaseService`, mirroring Jini's `Landlord.renewAll`/`LeaseMap`. Needed before Space meets real tuple volume regardless of centralization.

### Bugs fixed in the same pass (orthogonal to centralization, but touching the same code)

- **Cancel bypasses the cascade:** `LeaseManager::cancel` currently removes the lease record and broadcasts nothing, so a cancelled registry/space/event/txn resource is never told to clean up — it becomes immortal. Fix: broadcast a reclamation event on cancel too, with a `reason` field (`EXPIRED` vs `CANCELLED`), and have every cascade handler treat both the same way.
- **Cascade tasks die permanently on broadcast lag:** every cascade handler is shaped `while let Ok(lease) = rx.recv().await` — `broadcast::Receiver::recv()` returns `Err(Lagged(n))` under backpressure, and `while let Ok` silently treats that as stream-closed, permanently killing the task. Fix: match explicitly, `continue` (and reconcile) on `Lagged`, `break` only on a real close.
- **Duration vs. absolute-timestamp clock skew:** document `ttl_seconds` (the granted duration) as authoritative for renewal scheduling; `expires_at` is advisory/observational only, never something a client computes "renew when close to" against its own clock.

### CLI

`djinn lease` subcommand and the standalone `lease` container in `docker-compose.split.yml` are removed entirely — there's no process left to run. `coordin8 lease renew`/`cancel` (currently assumes one global LeaseMgr address) needs redesigning: likely folded into each resource subcommand group (`coordin8 registry renew --id`, `coordin8 space renew --id`, ...) rather than one generic top-level verb, since renewal is now inherently scoped to whichever service granted the lease. Exact shape is an implementation decision, not blocking this PRD.

### Go SDK (partial rework of what registry-bootstrap Phase 2 just shipped)

The `Leases()` global accessor and any `Registry.Lookup({interface: "LeaseMgr"})` path go away — there's no more `interface=LeaseMgr` to look up. Each resource client (`Registry()`, `Space()`, `Events()`) gains its own scoped renew/cancel, ideally returning a handle whose `.Renew(ctx, ttl)` is already bound to the right connection — closer to Jini's `Lease.renew()` than today's bare `lease_id` string, and a genuine ergonomic improvement, not just parity-matching. `KeepAlive` stops silently swallowing renewal failure (an error channel or `OnLeaseLost` callback, distinguishing transient transport failure from `LeaseExpired`/`LeaseNotFound`) — do this in the same pass since it's the same code path. This is real, acknowledged rework on top of an already-merged PR; small relative to the correction, and cheaper now than after Java/Node also build against the old shape.

### Java / Node SDKs

Untouched so far (Phases 3/4 of registry-bootstrap never started) — build them fresh against the corrected model once the Rust core and Go SDK land. This is exactly why pausing them now was the right call.

## Non-goals

- **A Norm-equivalent (third-party lease renewal service).** `grantor_endpoint` unblocks building one later; not building it now.
- **Full `LeaseMap`/`canBatch` client-side batching ergonomics.** `RenewAll` on the wire is in scope; a client-side manager that automatically groups renewals across services is not.
- **Changing Proxy or Registry's service-discovery role.** `RemoteCapabilityResolver`, `self_register`, `discover_txn_mgr`, `RemoteTxnEnlister` are unrelated to lease-centralization (they're legitimate, separate cross-service dependencies for application-service discovery) and are unaffected by this PRD.

## What gets removed

- `djinn lease` subcommand, `run_lease()`/`run_lease_on_listener*()` as a standalone process entry point, the `lease` service in `docker-compose.split.yml`, port 9001 as a concept.
- `coordin8-bootstrap`: `RemoteLeasing`, `PendingLeasing`, `LeaseSource`, `connect_direct`, `discover_lease_mgr`, `watch_expiry_prefix`, `watch_expiry_direct`. All of it was purpose-built for the centralized model.
- `COORDIN8_LEASE` env var, `COORDIN8_REGISTRY`-driven `interface=LeaseMgr` lookups anywhere they appear.
- The Layer-1 "LeaseMgr — Bedrock" row in CLAUDE.md's boot-order table. Boot order simplifies: every service becomes self-sufficient for its own leasing; Registry/Proxy/TxnMgr's *other* cross-service dependencies (service discovery, 2PC enlistment) are unaffected and keep their existing shape.
- All of registry-bootstrap Phase 1b's test scaffolding (`spawn_backbone_lease`, `COORDIN8_LEASE` wiring in ~10 integration tests) — replaced by each service's tests constructing their own embedded `LeaseManager` directly, the way `coordin8-event`/`coordin8-space`/`coordin8-txn`'s existing demo tests already do.

## Decisions

**No backward compatibility.** Same standing directive as registry-bootstrap: breaking proto/wire/API changes land outright, no dual code paths, no deprecated overloads. Pre-1.0, no external users.

**Bundle the breaking wire changes into one version bump.** `grantor_endpoint` addition, sentinel swap, and `RenewAll` all touch `lease.proto` — land together rather than three separate breaking changes.

**Phase 3/4 (Java/Node SDKs) stay paused** until this PRD's Rust core + Go SDK phases are done — confirmed explicitly by the user as the reason for pausing them.

## Suggested phases

1. **Rust core** — ✅ DONE. Per-service `LeaseManager` embedding (Registry, Space, EventMgr, TxnMgr), `LeaseService` mounted on each one's own server, standalone LeaseMgr process and everything in `coordin8-bootstrap` built for it removed, cancel-bypass and cascade-lag bugs fixed, per-namespace `LeaseConfig` (`LeaseConfig::from_env_for(namespace)`, `<NAMESPACE>_MAX_LEASE_TTL`/`<NAMESPACE>_PREFERRED_LEASE_TTL` overrides).
2. **Proto + wire** — ✅ DONE. `grantor_host`/`grantor_port` on `Lease`, `FOREVER`/`ANY` sentinel swap, `RenewAll`, `ReclaimReason` (`EXPIRED`/`CANCELLED`) on `ExpiryEvent`. Go stubs regenerated (`protoc-gen-go`/`protoc-gen-go-grpc` installed via Homebrew — `go install` was blocked by this environment's network policy). Node stub regen skipped (Node SDK untouched, Phase 4 of registry-bootstrap still paused).
3. **CLI** — ✅ DONE (pragmatic shape, see Decision below). `lease renew/cancel/grant/watch` now take `--grantor <host:port>`, defaulting to `--registry` (the common case — every self-registration's lease is Registry-granted). No per-resource subcommand split; kept the existing generic `lease` group since dialing-the-grantor-directly was the only change actually required.
4. **Go SDK** — ✅ DONE. `Leases()`/LeaseMgr-lookup path removed from `Connect()`. Added `DialLease(addr)` (dial any grantor directly) and `NewLeaseClient(conn)` (wrap a connection already held, e.g. `Client.RegistryLeases()` for the common case). `KeepAlive` now returns a `<-chan error`, distinguishing transient failure (keeps retrying) from `NotFound`/`FailedPrecondition` (stops, resource is genuinely gone) — fixes the silent-failure-swallowing bug. Fixed the two example services (`greeter_service`, `settlement-engine`) that used the old `Leases()` accessor.
5. **Re-validate** — ✅ DONE for what exists today. Full `cargo test --all` + `fmt` + `clippy` clean. Both `docker-compose.split.yml` (5 containers now, no more `lease` service) and `docker-compose.yml` (bundled monolith) rebuilt and live-verified: register/renew/cancel against Registry's and Space's independently-embedded LeaseManagers directly (proved isolation — a Space-granted lease ID is genuinely unknown to Registry, `NotFound`), hello-coordin8 Go greeter service+client round-trip in bundled mode. **Not yet re-validated:** auction-house (Java/Node participants), market-watch, double-entry against the new split-mode stack — those still need a pass.
6. **Then:** resume registry-bootstrap Phases 3 (Java SDK) and 4 (Node SDK) against the corrected model — still not started, by design.

## Bugs found and fixed while live-verifying (not originally scoped, surfaced by the rebuild)

- **`docker-compose.yml` (bundled) healthcheck used the gRPC Health Checking Protocol**, but `run_all()` never mounted a health service on any port (only split-mode services do — that was hardening-roadmap item 1's scope, bundled mode was never touched). Reverted to a plain TCP probe on the new port (9002, was 9001).
- **`docker-compose.yml`'s greeter service still set `DJINN_HOST`**, an env var the Go binary stopped reading after registry-bootstrap Phase 2 renamed it to `COORDIN8_REGISTRY` — a pre-existing gap from that earlier session (never live-tested against bundled mode's compose file after the rename). Fixed.

## Open questions — resolved during implementation

1. ~~Space's two lease namespaces~~ — kept as one `LeaseManager` with the existing `space:`/`space-watch:` resource_id prefix as an internal, Space-private convention (no longer a cross-service namespace scheme, just Space's own bookkeeping).
2. ~~CLI verb shape~~ — see Phase 3 above: `--grantor` flag, default to `--registry`.
3. ~~`ANY` sentinel's replacement value~~ — `LEASE_ANY = 0` (was `u64::MAX`), `LEASE_FOREVER = u64::MAX` (was `0`). Deliberately: the accidental proto3 default (`0`) now maps to the safe "ask for the server's preferred TTL" outcome instead of "grant forever."

## Remaining follow-ups

- Live-validate auction-house, market-watch, and double-entry against the rebuilt split-mode stack (they exercise EventMgr/TxnMgr more heavily than the smoke tests above did).
- CLI's `--grantor` UX could be improved further (e.g. surfacing the grantor address more visibly in `space write`/`event subscribe` output) — not blocking, noted for later polish.
- Node SDK stub regeneration was skipped this pass (needs `protoc-gen-ts_proto` from `sdks/node/node_modules`, not installed in this environment) — will need doing whenever Phase 4 (Node SDK) actually starts.
