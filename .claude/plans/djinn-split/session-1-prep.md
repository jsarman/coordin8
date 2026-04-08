# Session 1 Prep — Phase 0 Plumbing

**Goal:** Land the plumbing the split deployment needs, without changing any user-visible behavior yet. At the end of this session the monolith still boots exactly the same way it does today; a new `djinn lease` subcommand also boots just LeaseMgr; and there's a reusable `djinn-bootstrap` module that Phase 1+ will lean on.

## Scope (Phase 0)

Registry is the first service up. That's the natural bootstrap anchor — it's the one service that doesn't need to self-register, so it's also the one that can stand alone before anything else exists. Phase 0 brings up **both** `djinn registry` and `djinn lease` because the `djinn lease` integration test needs a real Registry to self-register against.

1. **Env vars** — `COORDIN8_REGISTRY`, `COORDIN8_BIND_ADDR`, `COORDIN8_ADVERTISE_ADDR`. Defaults:
   - `COORDIN8_REGISTRY` — unset in monolith mode, required for non-registry subcommands in split mode
   - `COORDIN8_BIND_ADDR` — `0.0.0.0:0` (OS-assigned port) by default in split subcommands
   - `COORDIN8_ADVERTISE_ADDR` — defaults to the bound address; override when running in Docker so peers dial the container name
2. **New crate: `djinn/crates/coordin8-bootstrap`**
   - Depends on `coordin8-core` + `coordin8-proto` (and `tonic`, `tokio`). This is why it's a new crate — `coordin8-core` must not pull in proto.
   - Public surface:
     - `discover_lease_mgr(registry_addr: &str) -> Result<LeaseServiceClient<Channel>>` — connects to Registry, looks up `interface=LeaseMgr`, retries with exponential backoff (start 100ms, cap 5s, forever), picks a healthy instance, returns a ready-to-use client
     - `self_register(registry: RegistryServiceClient, lease: LeaseServiceClient, interface: &str, attrs: HashMap<String,String>, advertise_addr: &str, ttl_seconds: u64) -> SelfRegistrationHandle` — grants a self-lease (`{interface}:self`), inserts the Registry entry under that lease, spawns a background renewal task at `ttl/3`, returns a handle whose `Drop` cancels the lease
     - `watch_expiry_prefix(lease: LeaseServiceClient, prefix: String, handler: impl Fn(ExpiryEvent) + Send + 'static)` — calls `WatchExpiry(resource_id="")`, prefix-filters client-side, reconnect loop with backoff (re-discovers LeaseMgr through Registry on reconnect). **Not used in Phase 0** beyond a compile check + unit tests — Phase 2+ will start calling it. Include it here so the crate is complete.
   - Doc comments on every public item. One integration test per public function where feasible.
3. **Multi-call subcommands on `coordin8-djinn`** (prefer `clap` with derive API; if `clap` is already a dep of anything in the workspace use that, otherwise manual `std::env::args` is fine — builder picks):
   - `djinn` (no subcommand) / `djinn all` — **exactly today's monolith behavior**. Zero regressions. This is the highest-priority invariant.
   - `djinn registry` — boots the Registry service alone. Binds `COORDIN8_BIND_ADDR`. No dependencies on any other service. **Does not self-register** (Registry is the well-known anchor; nothing to register with).
   - `djinn lease` — boots LeaseMgr alone. If `COORDIN8_REGISTRY` is set, uses `djinn-bootstrap::self_register` to register itself under `interface=LeaseMgr` with a 30s self-lease. If unset, logs a warning and runs standalone (useful for ad-hoc tests).
   - `djinn event`, `djinn space`, `djinn txn`, `djinn proxy` — stubs that log "split mode not yet implemented for <service>" and exit 1. Phase 2+ wires each one.
4. **Integration test** (new file `djinn/crates/coordin8-djinn/tests/split_phase0.rs` or similar):
   - Spawn `djinn registry` in a tokio task on an ephemeral port
   - Spawn `djinn lease` in another task, pointing `COORDIN8_REGISTRY` at the first
   - Connect a client to Registry, look up `interface=LeaseMgr`, assert one entry exists with the correct advertise address
   - Kill the lease task, wait for the self-lease TTL to elapse, re-lookup, assert the entry is gone (demonstrates the self-cleaning property)
   - This single test proves the whole Phase 0 contract end-to-end. If it's hard to make it fast (TTL has to be short) use a 3-second TTL in the test.

## Out of Scope for This Session

- Wiring the other services (event/space/txn/proxy) — each gets its own phase
- Any Docker compose changes — Phase 1 territory (compose with two containers + failover testing)
- Failover / redundancy testing — Phase 1 territory
- Full multi-service boot — Phase 6 territory
- SNS/SQS/cloud — phase 3 territory

## Critical Invariants

- **Zero regressions in the monolith.** `cargo run` with no subcommand boots everything exactly like today. Every existing test passes.
- **No `.proto` changes.** `WatchExpiry` already exists with `resource_id` filter support — no new RPCs.
- **Provider trait stays unchanged.** No stores touched.
- **`coordin8-core` must not depend on `coordin8-proto`** (current layering). If `djinn-bootstrap` needs gRPC clients, it must be its own crate sitting between core and the services.

## Key Files to Read First

- `djinn/crates/coordin8-djinn/src/main.rs` — current monolith wiring, lines 115–250 especially (broadcast setup + adapter tasks)
- `djinn/crates/coordin8-lease/src/service.rs` — `WatchExpiry` implementation (wraps the broadcast)
- `djinn/crates/coordin8-lease/src/manager.rs` — `LeaseManager::grant/renew/cancel` (you'll call these for self-lease)
- `djinn/crates/coordin8-registry/src/service.rs` — Registry insert/lookup RPCs (for self-registration and discover_lease_mgr)
- `proto/coordin8/lease.proto` + `proto/coordin8/registry.proto` — existing RPCs
- `sdks/go/coordin8/client.go` — reference for how a client connects (mirrors the pattern `djinn-bootstrap` should use on the Rust side)

## Success Criteria

- [ ] `cargo build --all` clean
- [ ] `cargo test --all` green (existing tests unaffected)
- [ ] `cargo clippy --all -- -D warnings` clean
- [ ] `cargo fmt --all --check` clean
- [ ] `cargo run --bin coordin8-djinn` still boots the monolith identically
- [ ] `cargo run --bin coordin8-djinn -- registry` boots Registry alone on `COORDIN8_BIND_ADDR`
- [ ] `cargo run --bin coordin8-djinn -- lease` boots LeaseMgr alone; with `COORDIN8_REGISTRY` set, self-registration works against a running Registry
- [ ] Phase 0 end-to-end integration test (registry + lease + self-clean) passes
- [ ] New `djinn-bootstrap` crate has doc comments on all three public functions

## Reference

- PRD: [`PRD.md`](./PRD.md)
- Endgame topology: [`../phase3-cloud-topology/resilience-plan.md`](../phase3-cloud-topology/resilience-plan.md)
- Project context: [`../../../CLAUDE.md`](../../../CLAUDE.md)
