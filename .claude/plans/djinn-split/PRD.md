# Djinn Split — PRD

## Goal

Split the monolithic Djinn binary into separately deployable services, prove they can boot independently, discover each other through Registry, and tolerate the loss of any single instance. Validate the entire model in `docker compose` against MiniStack before any cloud-target work begins.

The endgame topology (Lambda + Fargate + SNS/SQS) is already drafted in [`../phase3-cloud-topology/resilience-plan.md`](../phase3-cloud-topology/resilience-plan.md). **This PRD is the bridge.** It establishes the runtime contracts that make that endgame buildable: discovery, bootstrap, cross-service notifications, and redundancy patterns — without yet introducing any AWS-specific pub/sub.

## Motivation

- The monolith is a single failure domain. Lose the process, lose every service.
- Boot order is currently a strict in-process layering. To deploy services on different machines, the layering has to become discovery-driven instead of compile-time.
- Resilience patterns (run two LeaseMgrs, kill one, traffic continues) cannot be tested at all today.
- The cloud-topology endgame plan assumes services are already independent processes. This effort makes that assumption true.

## Non-Goals

- **No SNS/SQS, DynamoDB Streams, Lambda, or Fargate** in this effort. Those belong to phase 3. Cross-service notifications use gRPC streaming for now — simple, works in any deployment topology, no AWS dependency.
- **No protocol changes.** SDK clients are unchanged. The Registry/Lease/etc. proto contracts stay as they are.
- **No removal of the monolith binary.** `coordin8-djinn` continues to exist as a single-process boot for local dev and the existing examples. Split deployment is opt-in.

## Architectural Rules

### Rule 1 — Registry is the only well-known endpoint

Every service in a split deployment knows exactly one address from configuration: where Registry lives.

```bash
COORDIN8_REGISTRY=registry.coordin8.local:9002
```

Registry itself does not register with anything and is not leased — it just exists at its configured address. This avoids the bootstrap recursion and is how Jini's Lookup Service worked: the registrar is the one thing you have to know how to find; everything else is found through it.

### Rule 2 — Every other service registers with Registry, including LeaseMgr

LeaseMgr is no longer special. On boot it:

1. Connects to Registry
2. Self-leases: calls its own `Grant()` to issue a lease for `lease-mgr:self`
3. Registers a Registry entry under that lease, with capability `interface=LeaseMgr` and its actual bind address
4. The internal reaper renews the self-lease like any other lease

If the LeaseMgr process crashes, nobody renews → lease expires → Registry removes the entry → other services discover the new instance via Registry. Self-cleaning, fully consistent with how every other service expires.

### Rule 3 — Fixed port assignments are gone

The 9001/9002/9003/... layout was a convenience for in-process boot. In a split deployment each service binds whatever port the orchestrator hands it (or `0` for OS-assigned), and registers its actual address with Registry. The only port that matters operationally is Registry's.

### Rule 4 — Cross-service lease expiry is a gRPC stream

LeaseMgr already exposes a server-streaming RPC for this in `lease.proto`:

```proto
rpc WatchExpiry(WatchExpiryRequest) returns (stream ExpiryEvent);

message WatchExpiryRequest {
  // Empty = watch all expirations. Set resource_id to watch a specific one.
  string resource_id = 1;
}
```

**No proto changes required.** The existing RPC already wraps the `tokio::broadcast` channel on the server side (`coordin8-lease/src/service.rs::watch_expiry`).

In the monolith today, each downstream manager (Registry, EventMgr, Space, TxnMgr) spawns an adapter task in `coordin8-djinn/src/main.rs` that subscribes to the in-process broadcast and prefix-filters:

```rust
let mut rx = expiry_tx.subscribe();
tokio::spawn(async move {
    while let Ok(lease) = rx.recv().await {
        if lease.resource_id.starts_with("registry:") {
            /* call manager method */
        }
    }
});
```

The split-mode port of each adapter replaces `expiry_tx.subscribe()` with a gRPC `WatchExpiry(resource_id="")` client stream. Filter logic and manager calls are unchanged. If the stream drops, the service reconnects — looking LeaseMgr back up through Registry in case the instance moved.

This is the simplest cross-process replacement for `tokio::broadcast`. It's also a stepping stone: when phase 3 introduces SNS, the same downstream consumer code can switch to an SNS-backed stream behind a trait without touching manager logic.

### Rule 5 — Provider state stays per-service

Per-service stores are already isolated: only LeaseMgr reads the lease store, only Registry reads the registry store, etc. No store is touched by more than one service. The local provider keeps working in split mode — each service has its own DashMaps. The dynamo provider keeps working — each service connects independently. **No new provider work is required for the split itself.**

The one piece of shared state that *did* exist (`tokio::broadcast` channels) is replaced by Rule 4's gRPC streams.

## Binary Structure

Two options under consideration:

- **(a) Multi-call binary.** `djinn` becomes a dispatcher: `djinn lease`, `djinn registry`, `djinn event`, etc. Each subcommand boots exactly one service. The existing `djinn` (no subcommand) behavior is preserved as `djinn all` or default. Minimal Cargo churn — one bin target.
- **(b) One bin per service.** `coordin8-lease`, `coordin8-registry`, ... each as its own bin in its own crate. Cleaner separation, larger Dockerfile / mise / compose footprint.

**Recommendation:** start with (a). Easier to keep the monolith working alongside the split, easier to share boot helpers, easier to cut Dockerfiles. We can split into per-crate bins later if it actually helps.

## Phased Rollout

Each phase has the same shape: bring it up, prove it works, kill things on purpose, run two instances, prove failover. **Resilience is the deliverable** — code that hasn't survived a deliberate kill doesn't count as done.

### Phase 0 — Plumbing

- Add `COORDIN8_REGISTRY` env var (the one well-known endpoint) and `COORDIN8_BIND_ADDR` / `COORDIN8_ADVERTISE_ADDR` env vars read by each subcommand
- New `djinn-bootstrap` module (either a new small crate or a module under `coordin8-core`):
  - `discover_lease_mgr(registry_addr) -> LeaseServiceClient` — polls Registry for `interface=LeaseMgr`, retries with backoff, picks a healthy instance
  - `self_register(registry_client, lease_client, interface, attrs, advertise_addr, ttl)` — grants a self-lease, inserts the Registry entry, spawns a renewal loop, returns a handle that cancels the lease on drop
  - `watch_expiry_prefix(lease_client, prefix, handler)` — calls `WatchExpiry(resource_id="")`, filters by prefix, invokes the handler; reconnects on drop by re-discovering LeaseMgr through Registry
- Multi-call binary: add a subcommand layer to `coordin8-djinn` so `djinn lease`, `djinn registry`, `djinn event`, `djinn space`, `djinn txn`, `djinn proxy` each boot exactly one service. Default `djinn` (no subcommand) = `djinn all` = today's monolith behavior.
- **No proto changes.** `WatchExpiry` already exists and already streams expirations from the broadcast channel.

### Phase 1 — LeaseMgr + Registry

The smallest unit that exercises the architecture end-to-end.

- Two containers: `lease`, `registry`
- Registry comes up at its known address
- LeaseMgr boots, finds Registry via env var, self-leases, registers itself
- Validate: `coordin8 registry list` from the CLI shows the LeaseMgr entry
- **Failover test:** kill the LeaseMgr container. Watch the registry entry expire. Bring LeaseMgr back. Confirm a new entry appears.
- **Redundancy test:** run two LeaseMgr instances side by side. Both register. Kill one. The other's entry stays. Clients (via Registry lookup) keep getting served.

**Done when:** docker compose stack with `lease` + `registry` survives killing either LeaseMgr instance with no client-visible disruption.

### Phase 2 — EventMgr

- Add `event` container
- EventMgr finds LeaseMgr through Registry, grants its service lease, registers itself
- Subscribes to `WatchExpirations` with prefix `event:`
- Validate: market-watch demo runs against the split stack
- **Failover test:** kill EventMgr mid-stream, mailbox subscribers reconnect via Registry, recovered events drain
- **Redundancy test:** two EventMgr instances. Each handles a subset of subscribers. Kill one — its subscribers reconnect to the survivor.

### Phase 3 — Space

- Same shape. `space` container, `space:` and `space-watch:` expiry prefixes.
- Validate: the space tests in `coordin8-space/tests/demo.rs` run against the split stack
- Watch streams need careful thought — they're bound to a specific Space instance. After the split, when a watcher's Space dies, the client must reconnect through Registry.

### Phase 4 — TransactionMgr

- `txn` container. 2PC coordinator calls participants over gRPC — this already works across processes, no protocol change.
- Subscribes to `WatchExpirations` with prefix `txn:` for lease-expiry-driven aborts.
- **Failover test:** kill the coordinator mid-2PC. Verify participants don't hang indefinitely (the `txn:` lease expiry should drive them to abort).

### Phase 5 — Proxy

- `proxy` container. Already discovers upstreams through Registry — minimal changes to make it standalone.
- Reframe Proxy as an *optional edge service*, not part of the core boot. (Matches what the cloud-topology endgame plan says.)
- Validate: hello-coordin8 examples work with proxy as a separate container.

### Phase 6 — Stack Up Everything

- One docker compose file that brings up all services as separate containers
- All existing examples (hello-coordin8, market-watch, double-entry) run against it unchanged
- Run two of every service. Randomly kill one of each. Stack stays alive.

## What Stays the Same

- Proto contracts (only addition is `WatchExpirations`)
- SDK clients — they don't know whether they're talking to a monolith or a split deployment
- Provider trait + the stores in `coordin8-provider-local` and `coordin8-provider-dynamo`
- The monolith binary continues to exist for local dev and CI

## Open Questions

1. **Registry redundancy.** Phase 1 runs one Registry. To run two we either need (a) Registry to lean on the dynamo provider (already handles concurrent writers fine), or (b) some replication story for the local provider. For local provider in split mode, "one Registry" might just be a constraint.
2. **`WatchExpirations` semantics on reconnect.** If a downstream service drops its stream and reconnects, does it get expirations that happened during the gap? Two options: (a) "no, you missed them, do a reconciliation scan when you reconnect", (b) "LeaseMgr buffers recent expirations per prefix for N seconds". (a) is simpler, (b) is more reliable. Probably (a) plus a reconciliation pass.
3. **Self-lease TTL.** What's a reasonable TTL for LeaseMgr's own self-lease? Long enough to survive momentary GC pauses, short enough that a crashed LeaseMgr is detected quickly. Initial guess: 30s with 10s renewal.
4. **MiniStack ECS.** Phase 6 could optionally graduate from `docker compose` to MiniStack ECS task definitions to simulate cloud orchestration. Park this — `docker compose` is enough to prove the architecture, ECS is just packaging.
5. **Smart Proxy in the new world.** Already partially answered — it's an edge service. But: do we want any service-to-service traffic to go *through* the proxy (template-based failover), or do we trust orchestrator-level service discovery (env var → Registry → connect)? For Phase 0–6 the answer is "direct connect via Registry"; phase 3 endgame may revisit.
6. **Boot retries.** If a downstream service starts before LeaseMgr is registered, how long does it retry? Probably forever with exponential backoff — same as how containers wait on `depends_on: condition: service_healthy` today.

## Success Criteria

- All phases complete and demonstrated in `docker compose`
- Every example (hello-coordin8, market-watch, double-entry) runs against the fully-split stack with no client-side changes
- For each service, killing one instance with another running causes no client-visible failure
- The monolith binary still works for local dev and existing tests
- Phase 3 cloud-topology work can be picked up cleanly afterward (the broadcast-trait abstraction from Rule 4 is the seam)

## References

- [`../phase3-cloud-topology/resilience-plan.md`](../phase3-cloud-topology/resilience-plan.md) — endgame topology, picks up where this leaves off
- [`../../../CLAUDE.md`](../../../CLAUDE.md) — current monolith architecture
- [`../../../coordin8-design-napkin.md`](../../../coordin8-design-napkin.md) — full vision
