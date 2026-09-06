# Registry-Only Bootstrap — Session 1 Complete

**Branch:** `feat/registry-only-bootstrap` (not yet PR'd/merged)

## What shipped

- **Phase 1 (Rust, bundled mode):** `run_all()` now self-registers LeaseMgr/EventMgr/Proxy/TransactionMgr/Space into its own Registry — it never did before, so `Registry.Lookup()` silently returned nothing for every core service in monolith mode.
- **Phase 2 (Go SDK):** `Connect(host)` → `Connect(registryAddr)`. Looks up LeaseMgr/Proxy/Space/EventMgr via `Registry.Lookup({interface: "X"})` instead of assuming fixed ports on one host. Old semantics removed outright (pre-1.0, no back-compat shims). Also fixed a real bug in `ServiceDiscovery.Get()` — it hardcoded `"localhost:%d"` for a Proxy-forwarded port instead of deriving the host from the actual dialed address. Updated call sites: CLI (`--host` → `--registry`), hello-coordin8 Go service+client, auction-house settlement-engine.
- **Phase 1b (Rust, split mode):** fixed split-mode Registry's own lease bookkeeping being disconnected from the real LeaseMgr (see below) — the deepest and most subtle piece of this session.

## The Phase 1b bug and fix

Split-mode Registry (`djinn registry`) used a **private, local `LeaseManager`** to track TTLs for its own entries, completely disconnected from the real standalone LeaseMgr container. Every `lease_id` Registry handed back from `Register()` was meaningless to the real LeaseMgr — any client following the documented `Register()` → `Leases().KeepAlive()` pattern got `NotFound: lease not found` on the first renewal. Invisible until this session, because no SDK client could previously address split mode's Registry and LeaseMgr correctly at the same time (Phase 2 was the first thing to complete a real register-then-renew cycle against a live split stack).

The fix isn't a simple swap to `RemoteLeasing::connect()` — Registry can't discover its own LeaseMgr dependency through a Registry `Lookup`, since it *is* the Lookup service. Real chicken-and-egg: LeaseMgr's self-registration needs Registry's Leasing already resolved; Registry can only resolve LeaseMgr by looking it up in itself.

Investigated how Apache River (Jini's reference implementation) handles this in Reggie, its Lookup Service — finding: River never centralizes lease-granting at all. Every lease-granting service (Reggie, JavaSpaces, Mahalo) implements `Landlord` itself and manages its own lease table in-process; there's no separate "LeaseMgr" anything depends on. Coordin8's centralized LeaseMgr is a deliberate, already load-bearing departure from that (one `Leasing` trait, one `LeaseService` proto, one `Leases()` accessor in every SDK) — not something to undo for this fix. River's precedent doesn't hand over a drop-in answer for Registry specifically, but it clarifies the residual gap (below) is a general property of the whole push-based `WatchExpiry` design, not something new.

**Actual fix:** Registry is given LeaseMgr's address directly via a new `COORDIN8_LEASE` env var — mirroring how every other split-mode service is given Registry's address directly. `RemoteLeasing::connect_direct()` (dials LeaseMgr directly, no Registry lookup) and `watch_expiry_direct` (reconnects by redialing the direct address, not re-discovering through Registry) added to `coordin8-bootstrap`. `run_registry_on_listener` now gets the same serve-immediately/`PendingLeasing`/health-flip treatment as EventMgr/Space/TxnMgr/Proxy.

**Residual, accepted gap:** if the sole LeaseMgr dies *permanently* (not a normal restart at the same address, which Docker/k8s make the common case and which `connect_direct` already handles), Registry's own self-registered `interface=LeaseMgr` entry can't self-clean — nothing is left running to push the `WatchExpiry` event announcing its own expiry. Bounded in practice (the whole system is already degraded if LeaseMgr is truly gone) and general to `watch_expiry_prefix`'s reconnect-and-resume design, not unique to Registry. A future `ListLeases(prefix)` reconciliation RPC could close it system-wide if ever worth doing.

## Live verification

- `docker-compose.split.yml` rebuilt and brought up clean — all 6 containers healthy, still with **no `depends_on`** (boot-order independence intact).
- Registered a test capability via the Go CLI from a throwaway container on the split network, then renewed its lease directly against the real LeaseMgr: `FailedPrecondition: lease expired` on a stale attempt (proves the lease ID is real, just past its TTL — not `NotFound`), successful renewal with extended `expires_at` on a fresh attempt. This is the exact register-then-renew cycle that failed before the fix.

## Tests

All ~10 affected Rust integration tests updated for the new `run_registry_on_listener(listener, lease_addr)` signature. Most via a decoupled, always-alive "backbone" LeaseMgr instance dedicated to Registry's own dependency, kept separate from whatever LeaseMgr instance(s) each test kills/restarts to exercise unrelated failover behavior (`split_phase1.rs`, `split_remote_leasing.rs`, `split_chaos.rs`). `split_phase0.rs` needed no backbone — it's the one genuine single-instance scenario — and its original "self-clean after death" assertion turned out to still hold even post-fix in this in-process harness (aborting a `tokio::spawn`'d task doesn't stop its independently-spawned reaper or an already-open stream the way a real process kill would; see that file's doc comment).

`cargo test --all`, `cargo fmt --all --check`, `cargo clippy --all-targets --all-features` — all clean.

## Not started this session

Phase 3 (Java SDK), Phase 4 (Node SDK), Phase 5 (update every example's call sites, re-validate against both topologies, finally run auction-house on split mode — the original trigger for this whole effort).
