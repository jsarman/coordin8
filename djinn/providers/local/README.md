# coordin8-provider-local

The default in-memory provider. Implements every `*Store` trait from [`coordin8-core`](../../crates/coordin8-core/README.md) backed by `DashMap`. Zero dependencies on cloud services. Used for dev, edge, and tests.

## Where It Fits

Selected by the Djinn binary when `COORDIN8_PROVIDER` is unset or `local`. Holds no persistent state — restart the Djinn and everything is gone. For persistence use [`coordin8-provider-dynamo`](../dynamo/README.md).

## Layout

```
src/
  lib.rs                re-exports the five InMemory* stores
  lease_store.rs        InMemoryLeaseStore
  registry_store.rs     InMemoryRegistryStore
  event_store.rs        InMemoryEventStore
  txn_store.rs          InMemoryTxnStore
  space_store.rs        InMemorySpaceStore
```

## Build / Test

```bash
cargo build -p coordin8-provider-local
cargo test  -p coordin8-provider-local
```

## Notes

- All stores are `DashMap`-backed and `Send + Sync`
- No `init()` work needed — `::new()` is enough
- Mailboxes and event backlogs grow unbounded in memory; this is fine for dev but a concern for long-running instances
