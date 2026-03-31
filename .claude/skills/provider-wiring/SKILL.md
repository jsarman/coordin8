# provider-wiring

How to wire storage providers into the Djinn boot sequence. Load this skill when adding provider selection or a new provider backend.

## Current State

`main.rs` in `coordin8-djinn` hard-codes InMemory stores at Layer 0:

```rust
let lease_store = Arc::new(InMemoryLeaseStore::new());
let registry_store = Arc::new(InMemoryRegistryStore::new());
let event_store = Arc::new(InMemoryEventStore::new());
let txn_store = Arc::new(InMemoryTxnStore::new());
let space_store = Arc::new(InMemorySpaceStore::new());
```

All managers accept `Arc<dyn TraitName>`:
- `LeaseManager::new(store: Arc<dyn LeaseStore>, config: LeaseConfig)`
- `RegistryIndex::new(store: Arc<dyn RegistryStore>)`
- `EventManager::new(store: Arc<dyn EventStore>, ...)`
- `TxnManager::new(store: Arc<dyn TxnStore>, ...)`
- `SpaceManager::new(store: Arc<dyn SpaceStore>, ...)`

This means the stores are already behind trait objects — swapping providers is just changing which concrete type gets `Arc::new()`.

## Target Design

Environment variable: `COORDIN8_PROVIDER` (default: `local`)

```rust
let provider = std::env::var("COORDIN8_PROVIDER").unwrap_or_else(|_| "local".into());

// These are Arc<dyn TraitName> — the type erases the concrete provider
let (lease_store, registry_store, event_store, txn_store, space_store): (
    Arc<dyn LeaseStore>,
    Arc<dyn RegistryStore>,
    Arc<dyn EventStore>,
    Arc<dyn TxnStore>,
    Arc<dyn SpaceStore>,
) = match provider.as_str() {
    "dynamo" => {
        let client = coordin8_provider_dynamo::make_dynamo_client().await;

        let lease_store = Arc::new(coordin8_provider_dynamo::DynamoLeaseStore::new(client.clone()));
        lease_store.init().await?;

        let registry_store = Arc::new(coordin8_provider_dynamo::DynamoRegistryStore::new(client.clone()));
        registry_store.init().await?;

        // TODO: DynamoEventStore, DynamoTxnStore, DynamoSpaceStore not yet implemented
        // For now, fall back to InMemory for unimplemented stores
        let event_store = Arc::new(InMemoryEventStore::new());
        let txn_store = Arc::new(InMemoryTxnStore::new());
        let space_store = Arc::new(InMemorySpaceStore::new());

        info!("  ✓ Provider: dynamo (DynamoDB)");
        info!("    lease_store: DynamoDB");
        info!("    registry_store: DynamoDB");
        info!("    event_store: local (in-memory) — not yet implemented");
        info!("    txn_store: local (in-memory) — not yet implemented");
        info!("    space_store: local (in-memory) — not yet implemented");

        (lease_store, registry_store, event_store, txn_store, space_store)
    }
    _ => {
        let lease_store: Arc<dyn LeaseStore> = Arc::new(InMemoryLeaseStore::new());
        let registry_store: Arc<dyn RegistryStore> = Arc::new(InMemoryRegistryStore::new());
        let event_store: Arc<dyn EventStore> = Arc::new(InMemoryEventStore::new());
        let txn_store: Arc<dyn TxnStore> = Arc::new(InMemoryTxnStore::new());
        let space_store: Arc<dyn SpaceStore> = Arc::new(InMemorySpaceStore::new());
        info!("  ✓ Provider: local (in-memory)");
        (lease_store, registry_store, event_store, txn_store, space_store)
    }
};
```

## Files to Modify

### `djinn/crates/coordin8-djinn/Cargo.toml`
Add the dynamo provider dependency:
```toml
coordin8-provider-dynamo = { workspace = true }
```

### `djinn/crates/coordin8-djinn/src/main.rs`
1. Add import: `use coordin8_provider_dynamo;`
2. Add trait imports for type annotations: `use coordin8_core::{LeaseStore, RegistryStore, EventStore, SpaceStore, TxnStore};` (some may already be imported indirectly)
3. Replace the hard-coded Layer 0 block with the match block above
4. Everything after Layer 0 (managers, services, gRPC) stays IDENTICAL — it already works with `Arc<dyn Trait>`

### `docker-compose.yml`
Add to the `djinn` service environment:
```yaml
COORDIN8_PROVIDER: "${COORDIN8_PROVIDER:-local}"
DYNAMODB_ENDPOINT: "http://localstack:4566"
```

Add `depends_on` for localstack when using dynamo (optional — can be manual for now).

### `Dockerfile.djinn`
No changes needed — the binary is the same, provider is selected at runtime via env var.

## Important Constraints

- **Boot order is sacred.** Provider init (table creation) MUST complete before LeaseManager starts. The `.init().await?` calls in the dynamo branch handle this.
- **Hybrid mode is OK.** Using DynamoDB for lease+registry while event/txn/space stay InMemory is a valid intermediate state. Log which stores use which backend clearly.
- **Default is always `local`.** If `COORDIN8_PROVIDER` is unset or unrecognized, fall back to InMemory. Never panic on an unknown provider name.
- **ProxyManager takes `Arc<dyn RegistryStore>` directly** — it reads from the registry store for template resolution. Make sure the same `registry_store` instance is passed to both RegistryIndex and ProxyManager.

## Verification

After wiring:
1. `cargo check` from `djinn/` — must compile
2. `cargo run` with no env vars — must start with InMemory (existing behavior preserved)
3. `COORDIN8_PROVIDER=dynamo DYNAMODB_ENDPOINT=http://localhost:4566 cargo run` — must start with DynamoDB, create tables, and serve
