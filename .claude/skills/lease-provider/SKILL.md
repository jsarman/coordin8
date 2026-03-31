# lease-provider

Domain knowledge for implementing and modifying LeaseStore providers. Load this skill when working on lease-related storage backends.

## The Trait

```rust
// coordin8-core/src/lease.rs
pub const LEASE_FOREVER: u64 = 0;
pub const LEASE_ANY: u64 = u64::MAX;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeaseRecord {
    pub lease_id: String,
    pub resource_id: String,
    pub granted_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub ttl_seconds: u64,
}

#[async_trait]
pub trait LeaseStore: Send + Sync {
    async fn create(&self, resource_id: &str, ttl_secs: u64) -> Result<LeaseRecord, Error>;
    async fn renew(&self, lease_id: &str, ttl_secs: u64) -> Result<LeaseRecord, Error>;
    async fn cancel(&self, lease_id: &str) -> Result<(), Error>;
    async fn get(&self, lease_id: &str) -> Result<Option<LeaseRecord>, Error>;
    async fn get_by_resource(&self, resource_id: &str) -> Result<Option<LeaseRecord>, Error>;
    async fn list_expired(&self) -> Result<Vec<LeaseRecord>, Error>;
    async fn remove(&self, lease_id: &str) -> Result<(), Error>;
}
```

## Behavioral Contract

These behaviors MUST be identical across all providers. The InMemory implementation (`providers/local/src/lease_store.rs`) is the reference.

### create
- Generate a UUID `lease_id`
- `ttl_secs == 0` (LEASE_FOREVER): set `expires_at` to `DateTime::<Utc>::MAX_UTC`
- Otherwise: `expires_at = now + ttl_secs`
- Return the full `LeaseRecord`

### renew
- MUST return `Error::LeaseNotFound` if the lease doesn't exist
- Never silently create a new lease
- Update `expires_at` and `ttl_seconds`, preserve all other fields
- FOREVER renewal: same MAX_UTC logic as create

### cancel / remove
- Silent no-op if the lease doesn't exist (matches DashMap::remove behavior)
- `remove` delegates to `cancel`

### get
- Return `None` if not found, never error

### get_by_resource
- Return the first lease matching the `resource_id`
- Use an index if the backend supports it (GSI for DynamoDB)

### list_expired
- Return all records where `expires_at <= now`
- MUST exclude FOREVER leases (`ttl_seconds == 0`) — they never expire
- Boundary: `<=` (inclusive), not `<`

## Error Mapping

| Situation | Error Variant |
|-----------|---------------|
| Lease not found (on renew) | `Error::LeaseNotFound(id)` |
| Backend failure | `Error::Storage(message)` |
| Lease not found (on get) | Return `Ok(None)`, not an error |
| Cancel non-existent lease | Return `Ok(())`, not an error |

## FOREVER Lease Gotchas

FOREVER leases (`ttl_seconds == 0`) are load-bearing. Get them wrong and things silently break.

- `is_expired()` returns `false` for FOREVER leases (checked via `ttl_seconds == LEASE_FOREVER`)
- `list_expired()` must NEVER return a FOREVER lease
- **DynamoDB TTL**: Do NOT store epoch `0` in the TTL attribute. DynamoDB's native TTL reaper will garbage-collect items with past-epoch values. For FOREVER leases, OMIT the TTL attribute entirely.
- **expires_at** for FOREVER: use `DateTime::<Utc>::MAX_UTC` as a far-future sentinel

## LeaseConfig (Server-Side Policy)

```rust
pub struct LeaseConfig {
    pub max_ttl: Option<u64>,    // None = FOREVER allowed
    pub preferred_ttl: u64,       // Returned for LEASE_ANY requests
}
```

Negotiation: `LeaseConfig::negotiate(requested) -> granted_ttl`. This happens in the LeaseManager, NOT in the store. The store always receives the already-negotiated TTL.

## How Leases Wire Into the System

Leases are the bedrock — everything else depends on them:

- **Registry entries** have a `lease_id`. Lease expires → entry removed.
- **Event subscriptions** have a `lease_id`. Lease expires → subscription removed.
- **Transactions** have a `lease_id`. Lease expires → transaction auto-aborts.
- **Space tuples** have a `lease_id`. Lease expires → tuple removed.
- **Space watches** have a `lease_id`. Lease expires → watch removed.

The `resource_id` convention encodes the owner: `registry:{cap_id}`, `event:{sub_id}`, `txn:{txn_id}`, `space:{tuple_id}`, `space-watch:{watch_id}`.

## DynamoDB Table Schema

Table: `coordin8_leases`

| Attribute | Type | Role |
|-----------|------|------|
| `lease_id` | S | Hash key (PK) |
| `resource_id` | S | GSI hash key |
| `granted_at` | S | RFC 3339 timestamp |
| `expires_at` | S | RFC 3339 timestamp (lexicographic sort works) |
| `ttl_seconds` | N | The granted TTL (0 = FOREVER) |
| `ttl` | N | Epoch seconds for DynamoDB native TTL (OMIT for FOREVER) |

GSI: `resource_id-index` (PK: `resource_id`, projection: ALL)

## File Locations

| File | Purpose |
|------|---------|
| `djinn/crates/coordin8-core/src/lease.rs` | Trait + LeaseRecord + LeaseConfig |
| `djinn/crates/coordin8-core/src/error.rs` | Error enum |
| `djinn/crates/coordin8-lease/` | LeaseManager + reaper (consumes LeaseStore) |
| `djinn/providers/local/src/lease_store.rs` | InMemory reference implementation |
| `djinn/providers/dynamo/src/lease_store.rs` | DynamoDB implementation |
| `djinn/providers/dynamo/src/table.rs` | Table creation + GSI + TTL setup |

## Test Expectations

Every provider must pass these scenarios:
1. **create + get** — round-trip a lease, verify fields
2. **cancel removes** — cancel then get returns None
3. **expired shows in list** — 1s TTL, wait, verify in list_expired
4. **FOREVER never expires** — ttl=0, verify NOT in list_expired
5. **get_by_resource** — create, look up by resource_id
6. **renew updates expiry** — renew with longer TTL, verify expires_at moved forward

DynamoDB tests use `#[ignore]` and unique table names per run.
