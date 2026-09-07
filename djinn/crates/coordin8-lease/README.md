# coordin8-lease

`LeaseManager` + reaper — TTL-based liveness contracts. A **library**, not a standalone service: there is no LeaseMgr process and no lease port. Registry, EventMgr, Space, and TransactionMgr each construct their own `LeaseManager` and mount their own `LeaseService` on their own gRPC port, matching Jini/Apache River's `Landlord` pattern (every grantor manages its own leases in-process). See [`.claude/plans/distributed-leasing/PRD.md`](../../../.claude/plans/distributed-leasing/PRD.md) for the full rationale.

## Where It Fits

Each of Registry, EventMgr, Space, and TransactionMgr holds its own `Arc<LeaseManager>` and reacts to its own lease expiry as a coordination event:

- Registry entries with an expired lease are unregistered and broadcast as `EXPIRED`
- Event subscriptions are dropped
- Space tuples and watches are removed
- In-flight transactions are auto-aborted

Each service's own reaper task scans for its own expired leases on a fixed cadence and emits each one onto that service's own `tokio::sync::broadcast` channel, which the same service's other components subscribe to and route by `resource_id` prefix.

## Layout

```
src/
  lib.rs       re-exports LeaseManager + LeaseServiceImpl
  manager.rs   LeaseManager — grant / renew / cancel against a LeaseStore
  reaper.rs    background loop that finds expired leases and broadcasts them
  service.rs   tonic server impl for LeaseService (Grant, Renew, Cancel, WatchExpiry)
```

## Build / Test

```bash
cargo build -p coordin8-lease
cargo test  -p coordin8-lease
```

## Notes

- The reaper interval is `Duration::from_secs(1)` (set by whichever service constructs it — see `coordin8-djinn/src/services.rs`'s `embedded_landlord()`)
- `LeaseConfig` itself lives in `coordin8-core`, not here — `LeaseConfig::from_env_for(namespace)` honors `<NAMESPACE>_MAX_LEASE_TTL`/`<NAMESPACE>_PREFERRED_LEASE_TTL` (e.g. `SPACE_MAX_LEASE_TTL`), falling back to the global `MAX_LEASE_TTL`/`PREFERRED_LEASE_TTL` when the namespaced variable isn't set
- `LEASE_FOREVER` and `LEASE_ANY` constants live in `coordin8-core`
- A lease is self-describing: the `Lease` message carries `grantor_host`/`grantor_port`, so a holder always knows where to renew without prior knowledge of which service granted it
- Lease expiry is **not** an error — consumers should treat absence as a signal
