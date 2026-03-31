# registry-provider

Domain knowledge for implementing and modifying RegistryStore providers. Load this skill when working on registry-related storage backends.

## The Trait

```rust
// coordin8-core/src/registry.rs

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransportConfig {
    pub transport_type: String,
    pub config: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryEntry {
    pub capability_id: String,
    pub lease_id: String,
    pub interface: String,
    pub attrs: HashMap<String, String>,
    pub transport: Option<TransportConfig>,
}

#[async_trait]
pub trait RegistryStore: Send + Sync {
    async fn insert(&self, entry: RegistryEntry) -> Result<(), Error>;
    async fn update(&self, entry: RegistryEntry) -> Result<Option<RegistryEntry>, Error>;
    async fn remove_by_lease(&self, lease_id: &str) -> Result<Option<RegistryEntry>, Error>;
    async fn get(&self, capability_id: &str) -> Result<Option<RegistryEntry>, Error>;
    async fn get_by_lease(&self, lease_id: &str) -> Result<Option<RegistryEntry>, Error>;
    async fn list_all(&self) -> Result<Vec<RegistryEntry>, Error>;
}
```

## Behavioral Contract

The InMemory implementation (`providers/local/src/registry_store.rs`) is the reference. All providers must match.

### insert
- Stores the entry keyed by `capability_id`
- Maintains a lease index: `lease_id → capability_id` for fast expiry cleanup
- Overwrites if `capability_id` already exists (no upsert error)

### update
- If `capability_id` exists: update the entry, update lease index if `lease_id` changed, return `Some(entry)`
- If `capability_id` does NOT exist: return `None` (no error, no insert)
- Must clean up old lease index entry before inserting new one (lease_id may have changed)

### remove_by_lease
- Look up `capability_id` via the lease index
- Remove both the entry and the lease index mapping
- Return `Some(entry)` if found, `None` if no entry was associated with that lease
- This is the expiry path — called when a lease expires

### get
- Return `Some(entry)` by `capability_id`, or `None` if not found

### get_by_lease
- Look up `capability_id` via lease index, then fetch the entry
- Return `None` if no entry associated with that lease_id

### list_all
- Return all entries. No filtering, no ordering.

## Error Mapping

| Situation | Behavior |
|-----------|----------|
| Entry not found (get, get_by_lease) | Return `Ok(None)`, not an error |
| Entry not found (update) | Return `Ok(None)`, not an error |
| Entry not found (remove_by_lease) | Return `Ok(None)`, not an error |
| Backend failure | `Error::Storage(message)` |

The registry store never returns `ResourceNotFound` — it signals "not found" through `Option::None`.

## DynamoDB Table Schema

Table: `coordin8_registry`

| Attribute | Type | Role |
|-----------|------|------|
| `capability_id` | S | Hash key (PK) |
| `lease_id` | S | GSI hash key |
| `interface` | S | Service interface name |
| `attrs` | M | Attribute map (HashMap<String, String>) |
| `transport_type` | S | Transport protocol (e.g., "grpc") — only if transport is Some |
| `transport_config` | M | Transport config map — only if transport is Some |

GSI: `lease_id-index` (PK: `lease_id`, projection: ALL) — for `remove_by_lease` and `get_by_lease`

### DynamoDB Notes

- `attrs` is a DynamoDB Map (M) of String→String. Serialize as `AttributeValue::M(HashMap<String, AttributeValue::S>)`
- `transport` is optional. When `None`, omit `transport_type` and `transport_config` attributes entirely
- `update` should use `PutItem` with `condition_expression("attribute_exists(capability_id)")` — simpler than `UpdateItem` for a full replacement, and the condition prevents accidental inserts
- `remove_by_lease`: query the `lease_id-index` GSI first to get the `capability_id`, then `DeleteItem` by PK
- `list_all`: full table `Scan`. For v1 this is fine — registry is small. Note: DynamoDB scans return max 1MB per call. For correctness, handle `LastEvaluatedKey` pagination.

## How Registry Wires Into the System

- Each registry entry has a `lease_id` referencing a lease in the LeaseStore
- The `resource_id` convention for registry leases is `registry:{capability_id}`
- When a lease expires, the expiry cascade in `main.rs` calls `registry_index.unregister_by_lease(&lease.lease_id)` which calls through to `RegistryStore::remove_by_lease`
- The RegistryIndex (in `coordin8-registry` crate) wraps the RegistryStore and adds template matching logic
- Template matching (contains:, starts_with:, exact, Any) happens in the RegistryIndex, NOT in the store — the store is just CRUD

## File Locations

| File | Purpose |
|------|---------|
| `djinn/crates/coordin8-core/src/registry.rs` | Trait + RegistryEntry + TransportConfig |
| `djinn/crates/coordin8-core/src/error.rs` | Error enum |
| `djinn/crates/coordin8-registry/` | RegistryIndex + template matcher (consumes RegistryStore) |
| `djinn/providers/local/src/registry_store.rs` | InMemory reference implementation |
| `djinn/providers/dynamo/src/registry_store.rs` | DynamoDB implementation (to be created) |

## Test Expectations

Every provider must pass these scenarios:
1. **insert + get** — round-trip an entry, verify all fields including attrs and transport
2. **insert with None transport** — verify transport fields are omitted/handled
3. **update existing** — update attrs, verify returned entry reflects changes
4. **update non-existent** — returns None, does not insert
5. **remove_by_lease** — insert, remove by lease_id, verify get returns None
6. **remove_by_lease non-existent** — returns None, no error
7. **get_by_lease** — insert, look up by lease_id
8. **list_all** — insert multiple, verify all returned
9. **update changes lease_id** — update with new lease_id, verify old lease_id no longer resolves via get_by_lease

DynamoDB tests use `#[ignore]` and unique table names per run.
