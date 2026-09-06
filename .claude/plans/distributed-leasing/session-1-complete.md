# Distributed Leasing — Session 1 Complete

**Branch:** `worktree-distributed-leasing` (worktree at `.claude/worktrees/distributed-leasing`), based on `main` at `d5cc171` (post PR #25). Not yet committed/pushed as of this writeup — see next steps.

## What shipped

**Phase 1 (Rust core) — the architecture change itself.** Removed `LeaseMgr` as a centralized network service. Registry, Space, EventMgr, and TransactionMgr each now embed their own `coordin8_lease::LeaseManager` (own store, own `LeaseConfig`, own reaper) and mount `LeaseService` on their own gRPC server, alongside their primary service. This is Jini/Apache River's `Landlord` pattern: every grantor manages its own leases in-process. `coordin8-djinn/src/services.rs` got a shared `embedded_landlord(namespace, host, port)` helper used by all four, and a `spawn_cascade` helper that fixes the cascade-lag bug (`RecvError::Lagged` now logs and continues instead of silently, permanently killing the cascade task — the old `while let Ok(...)` shape). `coordin8-lease::LeaseManager::cancel()` now also broadcasts a reclamation event (fixes the cancel-bypasses-cascade bug — verified safe across all four services since each already does its own store removal before calling `cancel()`, making the cascade-triggered handler a safe no-op).

**Phase 2 (proto + wire) — bundled with Phase 1 since it touched the same files.** `Lease` gained `grantor_host`/`grantor_port` (self-describing leases — no single well-known address to renew against anymore). `ExpiryEvent` gained a `reason` (`EXPIRED`/`CANCELLED`). Added `RenewAll` for batch renewal. Swapped the `FOREVER`/`ANY` sentinels: `LEASE_ANY = 0` (was `u64::MAX`), `LEASE_FOREVER = u64::MAX` (was `0`) — the accidental proto3 default now maps to the safe outcome. Added `LeaseConfig::from_env_for(namespace)` for per-namespace TTL policy overrides.

**Phase 3/4 (CLI + Go SDK) — done far enough to keep everything working, not deeply polished.** The Go SDK's `Connect()` no longer looks up a nonexistent "LeaseMgr" interface (this was found live — it broke every CLI subcommand, not just lease ones, so fixing it wasn't optional). `Client.Leases()` is gone; replaced by `coordin8.DialLease(addr)` (dial any grantor directly) and `NewLeaseClient(conn)` / `Client.RegistryLeases()` (reuse a connection already held). `KeepAlive` now returns a `<-chan error` distinguishing transient failure from `NotFound`/`FailedPrecondition` — fixes the silent-failure-swallowing bug from the research doc. CLI's `lease` subcommand group gained a `--grantor` flag (defaults to `--registry`, the common case).

## What this reverses from registry-bootstrap

Phase 1b of `.claude/plans/registry-bootstrap/` (the `COORDIN8_LEASE` direct-dial fix, `RemoteLeasing`/`PendingLeasing`/`connect_direct`) is now dead code, removed. That whole mechanism existed only to let Registry reach an external LeaseMgr it depended on — once leasing is embedded, the dependency (and the self-referential bootstrap-cycle problem it created) disappears entirely. Also removed: `djinn lease` subcommand, port 9001 as a concept, all of registry-bootstrap's `spawn_backbone_lease` test scaffolding (~10 integration tests rewritten or deleted).

## Two real bugs found and fixed while live-verifying (not originally scoped)

1. `docker-compose.yml`'s (bundled monolith) healthcheck was upgraded to the gRPC Health Checking Protocol during this session, but `run_all()` never mounted a health service on any port — that was only ever wired for split mode (hardening-roadmap item 1). Reverted to a plain TCP probe on the new port.
2. `docker-compose.yml`'s greeter service still set `DJINN_HOST`, an env var the Go binary stopped reading after registry-bootstrap Phase 2 renamed it to `COORDIN8_REGISTRY` — bundled mode was apparently never live-tested against the compose file after that rename. Fixed.

## Live verification

- Full `cargo test --all`, `cargo fmt --all --check`, `cargo clippy --all-targets --all-features` — all clean.
- `docker-compose.split.yml` rebuilt: 5 containers now (no more `lease` service), all healthy, no `depends_on`.
- **Proof of true per-service isolation**: registered a capability against split-mode Registry, renewed its lease directly against Registry (`FailedPrecondition`/success as expected). Granted a lease directly against Space's embedded LeaseManager, renewed it against Space. Then tried renewing the *Space*-granted lease against *Registry* — got `NotFound`, proving Registry has zero knowledge of Space's leases. No shared state anywhere.
- `docker-compose.yml` (bundled monolith) rebuilt: both containers healthy, greeter self-registers (via Registry's embedded LeaseManager) and successfully keeps its lease alive in a loop, greeter_client round-trips a real "Hello, World!" through Proxy.

## Environment note

`protoc-gen-go`/`protoc-gen-go-grpc` needed installing to regenerate Go stubs — `go install` was blocked by this sandbox's network policy (404s from the Go module proxy); `brew install protoc-gen-go protoc-gen-go-grpc` worked. Node stub regeneration (`sdks/node`) was skipped — needs `node_modules` installed there, and Node SDK work is out of scope until Phase 4 of registry-bootstrap resumes.

## Not done this session

Auction-house, market-watch, and double-entry examples not yet re-validated against the rebuilt split-mode stack (they exercise EventMgr/TxnMgr more than the smoke tests above did). Java/Node SDKs untouched, as planned — registry-bootstrap Phases 3/4 stay paused until their own dedicated pass.

## Next steps

1. Commit this work (currently uncommitted in the worktree) and open a PR.
2. Live-validate the remaining examples (auction-house, market-watch, double-entry) against the new split-mode stack.
3. Resume registry-bootstrap Phases 3 (Java SDK) and 4 (Node SDK) against the corrected, distributed-leasing model.
