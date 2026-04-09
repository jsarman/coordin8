# Djinn Split — Completion Notes

**Merged to main:** 2026-04-09 (PR jsarman/coordin8#4)

## What shipped

All six phases of the PRD, plus chaos testing that wasn't originally scoped but became the centerpiece proof.

### Phase 0 — Plumbing
- New `coordin8-bootstrap` crate with `discover_lease_mgr`, `self_register`, `watch_expiry_prefix`, plus `RemoteLeasing` and `RemoteCapabilityResolver` clients
- Env vars: `COORDIN8_REGISTRY`, `COORDIN8_BIND_ADDR`, `COORDIN8_ADVERTISE_ADDR`
- Multi-call subcommands on `coordin8-djinn`: `djinn`, `djinn all`, `djinn lease`, `djinn registry`, `djinn event`, `djinn space`, `djinn txn`, `djinn proxy`
- Bundled boot path is unchanged — zero regressions on existing tests

### The seams (the real architectural win)
Two traits in `coordin8-core` decouple services from whether they're bundled or split:

- **`Leasing`** — `LocalLeasing` wraps the in-process `LeaseManager`; `RemoteLeasing` wraps a gRPC client with transparent reconnect + retry on transport failure. `call_with_retry` clones the client out of its Mutex before `.await` so the lock isn't held across suspension points. `retry_forever` loops on `Unavailable | Cancelled | Unknown | DeadlineExceeded`, reconnecting between attempts — which re-resolves LeaseMgr through Registry if the instance moved.
- **`CapabilityResolver`** — same pattern. `RemoteCapabilityResolver::lookup_with_retry` mirrors `RemoteLeasing::call_with_retry` (this was fixed during the simplify pass — original version held the Mutex across `.await`).

Every service in split mode takes a `Box<dyn Leasing>` and a `Box<dyn CapabilityResolver>`. Manager code is identical in both modes.

### Phases 1–5
- **Phase 1** — `djinn lease` and `djinn registry` split cleanly, self-registering with a short TTL so dead instances age out of Registry quickly
- **Phase 2** — `djinn event` uses `watch_expiry_prefix` with `event:` prefix
- **Phase 3** — `djinn space` uses `space:` and `space-watch:` prefixes
- **Phase 4** — `djinn txn` uses `txn:` prefix
- **Phase 5** — `djinn proxy` already discovers through Registry; minimal changes to standalone

### Chaos tests (new, beyond original scope)
`djinn/crates/coordin8-djinn/tests/split_chaos.rs` — two 3-phase tests:

- **`chaos_remote_leasing_survives_leasemgr_kill`** — pins `RemoteLeasing` to lease_a via an initial grant, spawns lease_b, kills lease_a, proves phase 1 (failover) keeps succeeding through lease_b; then kills lease_b and proves phase 2 errors cleanly instead of silently succeeding against a zombie.
- **`chaos_split_space_survives_leasemgr_kill`** — same 3-phase pattern but drives a real Space Write loop through the full split stack.

Both assert `phase1 > phase0` (failover worked) and `phase2_errors > phase2_successes` (once every LeaseMgr is gone, operations fail instead of silently succeeding). Results at time of merge:

```
chaos A: phase0=15 phase1=27 phase2=0 phase2_errors=10
chaos B: phase0=17 phase1=87 phase2=0 phase2_errors=10
```

## Decisions worth preserving

- **`serve_with_incoming_shutdown` over `serve_with_incoming`.** A plain `JoinHandle::abort()` is a no-op on tonic servers because per-connection HTTP/2 tasks are detached. The chaos tests needed a graceful-shutdown path (`run_lease_on_listener_with_shutdown`) that actually closes the listener.
- **500ms timeout on grant/cancel in the chaos driver.** `retry_forever` has no surviving LeaseMgr after phase 2 starts, so grants hang forever without a timeout. Wrapping the driver ops (not the `Leasing` impl itself) keeps the trait semantics intact.
- **500ms timeout + abort fallback on `LeaseHandle::kill()`.** Tonic's graceful shutdown waits for in-flight RPCs to drain, but our in-flight Write was stuck inside `retry_forever` on the upstream we just killed. Timeout-then-abort unwedges it.
- **3-phase success/error counting beats latency assertions.** First attempt used `max_latency_ms` but registry happened to return lease_b fast enough (48ms) to make the threshold meaningless. Counting successes per phase is robust.
- **Pinning `RemoteLeasing` to a specific LeaseMgr.** Chaos test A issues an initial grant before lease_b comes up, forcing the resolver cache to lease_a. Without this, the test doesn't actually prove failover — the resolver could have picked lease_b from the start.

## Follow-ups (tracked in master PRD, priority #4)

1. **Docker-compose chaos** — kill a container, not a tokio task. Would catch orchestrator-level surprises the in-process test can't.
2. **DynamoDB/LocalStack provider-swap test** — prove the `Leasing` + `CapabilityResolver` seams work across a real provider boundary, not just the local provider.
3. **Registry redundancy in split mode** — currently one Registry. PRD open question #1 proposed either (a) lean on Dynamo's concurrent-writer story or (b) replication for local. Still unanswered.

## Key files

- `djinn/crates/coordin8-core/src/lease.rs` — `Leasing` trait
- `djinn/crates/coordin8-core/src/resolver.rs` — `CapabilityResolver` trait
- `djinn/crates/coordin8-bootstrap/src/lib.rs` — `RemoteLeasing`, `RemoteCapabilityResolver`, `discover_lease_mgr`, `self_register`, `watch_expiry_prefix`
- `djinn/crates/coordin8-djinn/src/services.rs` — split subcommand boot helpers (`run_lease_on_listener_with_shutdown` etc.)
- `djinn/crates/coordin8-djinn/tests/split_chaos.rs` — chaos tests
