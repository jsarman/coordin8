# coordin8-core

Shared types and storage traits used by every other crate in the workspace. No I/O, no async runtime, no gRPC — pure data + trait definitions.

## Where It Fits

This is the bottom of the dependency graph. Each service crate (`coordin8-lease`, `coordin8-registry`, etc.) depends on `coordin8-core` for the types it operates on, and each provider (`coordin8-provider-local`, `coordin8-provider-dynamo`) implements the `*Store` traits defined here. The Djinn binary type-erases concrete providers as `Arc<dyn LeaseStore>` etc., so swapping `local` for `dynamo` is a one-line change.

## Layout

```
src/
  lib.rs        re-exports the public surface
  error.rs      shared Error type
  lease.rs      LeaseRecord, LeaseStore, LeaseConfig, LEASE_ANY, LEASE_FOREVER
  registry.rs   RegistryEntry, RegistryStore, TransportConfig
  event.rs      EventRecord, SubscriptionRecord, EventStore, DeliveryMode
  txn.rs        TransactionRecord, ParticipantRecord, PrepareVote, TransactionState, TxnStore
  space.rs      TupleRecord, SpaceWatchRecord, SpaceEventKind, SpaceStore
```

## Build / Test

```bash
cargo build -p coordin8-core
cargo test  -p coordin8-core
```

## Notes

- `*Store` traits are `async_trait` and `Send + Sync` — required so the Djinn can hold them as `Arc<dyn>`
- `LeaseConfig::from_env()` reads `COORDIN8_LEASE_MAX_TTL` and `COORDIN8_LEASE_PREFERRED_TTL`
- The traits are intentionally narrow — provider implementations should not need to know about gRPC or service-layer logic
