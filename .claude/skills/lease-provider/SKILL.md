---
name: lease-provider
description: Domain knowledge for the LeaseStore trait and its InMemory/DynamoDB providers, including the LEASE_ANY/LEASE_FOREVER sentinels. Use when implementing or modifying LeaseStore, anything touching lease expiry (list_expired, the reaper, lease tables), TTL negotiation, or FOREVER handling.
---

# lease-provider

Domain knowledge for implementing and modifying LeaseStore providers. Load this skill when working on lease-related storage backends.

> **FOREVER is `u64::MAX`, ANY is `0`.** Not the other way around. `ttl_seconds == 0` means "client deferred to the server's preferred TTL" and is never stored on a record (the manager negotiates it to a real TTL first). Code or docs that say `ttl_seconds == 0` means FOREVER are stale and have already caused a production bug (Dynamo `list_expired`).

## The Trait

```rust
// coordin8-core/src/lease.rs
pub const LEASE_ANY: u64 = 0;            // proto3 default-unset uint64 -> safe, bounded, server-chosen TTL
pub const LEASE_FOREVER: u64 = u64::MAX; // explicit "never expires" (only honored if LeaseConfig.max_ttl is None)

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
- `ttl_secs == LEASE_FOREVER` (`u64::MAX`): set `expires_at` to `DateTime::<Utc>::MAX_UTC`
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
- MUST exclude FOREVER leases (`ttl_seconds == LEASE_FOREVER`, i.e. `u64::MAX`) — they never expire
- Boundary: `<=` (inclusive), not `<`
- InMemory gets FOREVER exclusion for free by comparing `DateTime<Utc>` values (`MAX_UTC` is never `<= now`). **A Dynamo scan that compares `expires_at` as an RFC3339 string does NOT**: `MAX_UTC` formats as `+262142-12-31T23:59:59.999999999+00:00`, and `+` (0x2B) sorts before any digit, so FOREVER leases compare as "less than now" and would be reaped. Exclude them explicitly on `ttl_seconds <> :forever` with `:forever = "18446744073709551615"` (a DynamoDB Number holds 38 digits, so this is exact), or compare a numeric epoch attribute.
- A Dynamo implementation must paginate the scan (`last_evaluated_key` / `exclusive_start_key` loop) — one scan page is at most 1 MB, and an unpaginated `list_expired` silently misses expired leases once the table grows.
- Red flag: a filter containing `ttl_seconds <> :zero` / `ttl_seconds = 0` is the stale-skill bug — it excludes nothing real and includes FOREVER.

## Error Mapping

| Situation | Error Variant |
|-----------|---------------|
| Lease not found (on renew) | `Error::LeaseNotFound(id)` |
| Backend failure | `Error::Storage(message)` |
| Lease not found (on get) | Return `Ok(None)`, not an error |
| Cancel non-existent lease | Return `Ok(())`, not an error |

## FOREVER Lease Gotchas

FOREVER leases (`ttl_seconds == u64::MAX`) are load-bearing. Get them wrong and things silently break.

- `is_expired()` returns `false` for FOREVER leases (checked via `ttl_seconds == LEASE_FOREVER`)
- `list_expired()` must NEVER return a FOREVER lease
- Never compare `expires_at` as RFC3339 strings when FOREVER/`MAX_UTC` is possible (see `list_expired`)
- `now + Duration::seconds(ttl as i64)` must never be computed with `ttl == u64::MAX` — branch on FOREVER first
- Every DynamoDB Scan/Query must paginate `LastEvaluatedKey`
- **DynamoDB TTL**: Do NOT store epoch `0` in the TTL attribute. DynamoDB's native TTL reaper will garbage-collect items with past-epoch values. For FOREVER leases, OMIT the TTL attribute entirely.
- **expires_at** for FOREVER: use `DateTime::<Utc>::MAX_UTC` as a far-future sentinel

## LeaseConfig (Server-Side Policy)

```rust
pub struct LeaseConfig {
    pub max_ttl: Option<u64>,    // None = FOREVER allowed
    pub preferred_ttl: u64,       // Returned for LEASE_ANY requests
}
```

Negotiation: `LeaseConfig::negotiate(requested) -> granted_ttl`: `LEASE_ANY` -> `preferred_ttl`; `LEASE_FOREVER` -> `u64::MAX` if `max_ttl` is `None`, else capped to `max_ttl`; any other TTL -> `min(ttl, max_ttl)`. This happens in the LeaseManager, NOT in the store. The store always receives the already-negotiated TTL.

Policy env vars (read per service via `LeaseConfig::from_env_for(Some(namespace))`): `MAX_LEASE_TTL` (seconds or `FOREVER`, default 3600), `PREFERRED_LEASE_TTL` (default 300), each overridable per namespace as `<NS>_MAX_LEASE_TTL` / `<NS>_PREFERRED_LEASE_TTL` (`REGISTRY_`, `EVENT_`, `SPACE_`, `TXN_`).

## How Leases Wire Into the System

Leasing is distributed: each of Registry, EventMgr, Space, and TransactionMgr embeds its own `LeaseManager` (own store, own reaper) and mounts `LeaseService` on its own port; a `Lease` carries `grantor_host`/`grantor_port` so holders renew against the grantor. There is no LeaseMgr service. The `Leasing` trait (`grant`/`renew`/`cancel`) only decouples downstream code from the concrete `LeaseManager`.

Every leased resource is tied to a lease:

- **Registry entries** have a `lease_id`. Lease expires → entry removed.
- **Event subscriptions** have a `lease_id`. Lease expires → subscription removed.
- **Transactions** have a `lease_id`. Lease expires → transaction auto-aborts.
- **Space tuples** have a `lease_id`. Lease expires → tuple removed.
- **Space watches** have a `lease_id`. Lease expires → watch removed.

The `resource_id` convention encodes the owner: `registry:{cap_id}`, `event:{sub_id}`, `txn:{txn_id}`, `space:{tuple_id}`, `space-watch:{watch_id}`.

## DynamoDB Table Schema

Leasing is distributed — there's no single shared table. Each service that grants leases (Registry, EventMgr, Space, TransactionMgr) owns its own table, named `coordin8_leases_{namespace}` (e.g. `coordin8_leases_registry`) by `lease_store_from_env()` in `coordin8-djinn/src/services.rs`. Same schema in each:

| Attribute | Type | Role |
|-----------|------|------|
| `lease_id` | S | Hash key (PK) |
| `resource_id` | S | GSI hash key |
| `granted_at` | S | RFC 3339 timestamp |
| `expires_at` | S | RFC 3339 timestamp (lexicographic sort works) |
| `ttl_seconds` | N | The granted TTL (`18446744073709551615` = FOREVER) |
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
4. **FOREVER never expires** — ttl=`LEASE_FOREVER` (`u64::MAX`), verify NOT in list_expired (and that a real expired lease alongside it IS returned)
5. **get_by_resource** — create, look up by resource_id
6. **renew updates expiry** — renew with longer TTL, verify expires_at moved forward

DynamoDB tests use `#[ignore]` and unique table names per run. Because they are `#[ignore]`, CI does not run them — run `cd djinn && cargo test -p coordin8-provider-dynamo lease_store -- --ignored` against MiniStack (`docker compose up ministack`) yourself.
