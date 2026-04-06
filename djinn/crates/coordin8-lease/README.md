# coordin8-lease

LeaseMgr — TTL-based liveness contracts. The bedrock of every other service in the Djinn.

## Where It Fits

Layer 1 in the boot sequence (port `9001`). Registry, EventMgr, Space, and TransactionMgr all hold an `Arc<LeaseManager>` and react to lease expiry as a coordination event:

- Registry entries with an expired lease are unregistered and broadcast as `EXPIRED`
- Event subscriptions are dropped
- Space tuples and watches are removed
- In-flight transactions are auto-aborted

A reaper task scans for expired leases on a fixed cadence and emits each one onto a `tokio::sync::broadcast` channel that subscribers route by `resource_id` prefix.

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

- The reaper interval is `Duration::from_secs(1)` (set in `coordin8-djinn`)
- `LeaseConfig::from_env()` honors `COORDIN8_LEASE_MAX_TTL` and `COORDIN8_LEASE_PREFERRED_TTL`
- `LEASE_FOREVER` and `LEASE_ANY` constants live in `coordin8-core`
- Lease expiry is **not** an error — consumers should treat absence as a signal
