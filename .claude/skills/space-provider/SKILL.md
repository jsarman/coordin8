# space-provider

Domain knowledge for implementing and modifying SpaceStore providers. Load this skill when working on tuple store and watch storage backends.

**This is the largest and most complex store.** 15 methods, transaction isolation buffers, template matching, and watch subscriptions. Read carefully.

## The Trait

```rust
// coordin8-core/src/space.rs

#[derive(Debug, Clone)]
pub struct TupleRecord {
    pub tuple_id: String,
    pub attrs: HashMap<String, String>,
    pub payload: Vec<u8>,
    pub lease_id: String,
    pub written_by: String,
    pub written_at: DateTime<Utc>,
    pub input_tuple_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SpaceWatchRecord {
    pub watch_id: String,
    pub template: HashMap<String, String>,
    pub on: SpaceEventKind,
    pub lease_id: String,
    pub handback: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SpaceEventKind {
    Appearance,
    Expiration,
}

#[async_trait]
pub trait SpaceStore: Send + Sync {
    // ── Tuple CRUD ──
    async fn insert(&self, record: TupleRecord) -> Result<(), Error>;
    async fn insert_uncommitted(&self, txn_id: &str, record: TupleRecord) -> Result<(), Error>;
    async fn get(&self, tuple_id: &str) -> Result<Option<TupleRecord>, Error>;
    async fn remove(&self, tuple_id: &str) -> Result<Option<TupleRecord>, Error>;
    async fn remove_by_lease(&self, lease_id: &str) -> Result<Option<TupleRecord>, Error>;

    // ── Template matching ──
    async fn find_match(&self, template: &HashMap<String, String>, txn_id: Option<&str>) -> Result<Option<TupleRecord>, Error>;
    async fn take_match(&self, template: &HashMap<String, String>, txn_id: Option<&str>) -> Result<Option<TupleRecord>, Error>;
    async fn find_all_matches(&self, template: &HashMap<String, String>, txn_id: Option<&str>) -> Result<Vec<TupleRecord>, Error>;

    // ── Transaction lifecycle ──
    async fn commit_txn(&self, txn_id: &str) -> Result<Vec<TupleRecord>, Error>;
    async fn abort_txn(&self, txn_id: &str) -> Result<(Vec<TupleRecord>, Vec<TupleRecord>), Error>;
    async fn has_txn(&self, txn_id: &str) -> Result<bool, Error>;

    // ── General ──
    async fn list_all(&self) -> Result<Vec<TupleRecord>, Error>;

    // ── Watch subscriptions ──
    async fn create_watch(&self, watch: SpaceWatchRecord) -> Result<(), Error>;
    async fn remove_watch(&self, watch_id: &str) -> Result<(), Error>;
    async fn remove_watch_by_lease(&self, lease_id: &str) -> Result<Option<SpaceWatchRecord>, Error>;
    async fn list_watches(&self) -> Result<Vec<SpaceWatchRecord>, Error>;
}
```

## Behavioral Contract

The InMemory implementation (`providers/local/src/space_store.rs`) is the reference.

### Tuple Operations

- **insert**: committed (visible) store. Maintain `lease_id → tuple_id` index.
- **insert_uncommitted**: buffer under `txn_id`. Invisible to other transactions and non-transactional reads.
- **get**: by `tuple_id`, committed store only. Returns `None` if not found.
- **remove**: by `tuple_id`, returns the removed record. Cleans up lease index.
- **remove_by_lease**: look up via lease index, remove. Returns record or `None`.

### Template Matching

Uses `coordin8_registry::matcher::{matches, parse_template}` — the same template matching logic as Registry. Operators: exact, `contains:`, `starts_with:`, Any (missing field).

- **find_match**: search committed first, then txn's uncommitted if `txn_id` provided. Non-destructive.
- **take_match**: search txn's uncommitted first (remove from buffer), then committed (remove from store, track for restore-on-abort). Under a txn, taken tuples go into `txn_taken` buffer.
  - **Take atomicity**: in DynamoDB, use `DeleteItem` with `ConditionExpression: "attribute_exists(tuple_id)"` — exactly one winner in a race.
- **find_all_matches**: search committed + uncommitted. Empty template matches everything.

### Transaction Lifecycle

- **commit_txn**: flush uncommitted writes to committed store (insert each). Clear txn_taken buffer (takes are finalized). Return the flushed tuples (for broadcasting).
- **abort_txn**: discard uncommitted writes. Restore txn_taken tuples back to committed store. Return `(discarded, restored)`.
- **has_txn**: true if any uncommitted writes OR taken tuples exist for this txn.

### Watch Operations

- **create_watch**: store watch, maintain `lease_id → watch_id` index.
- **remove_watch**: remove by `watch_id`, clean up lease index. Silent if not found.
- **remove_watch_by_lease**: look up via lease index, remove. Return watch or `None`.
- **list_watches**: return all watches.

## DynamoDB Table Schema

### Committed tuples: `coordin8_space`

| Attribute | Type | Role |
|-----------|------|------|
| `tuple_id` | S | Hash key (PK) |
| `attrs` | M | HashMap<String, String> |
| `payload` | B | Binary blob |
| `lease_id` | S | |
| `written_by` | S | |
| `written_at` | S | RFC 3339 timestamp |
| `input_tuple_id` | S | Optional — omit if None |

GSI: `lease_id-index` (PK: `lease_id`) for `remove_by_lease`

### Uncommitted buffer: `coordin8_space_uncommitted`

| Attribute | Type | Role |
|-----------|------|------|
| `txn_id` | S | Hash key (PK) — groups by transaction |
| `tuple_id` | S | Sort key (SK) — unique within txn |
| (same attributes as committed table) | | |

### Txn-taken buffer: `coordin8_space_txn_taken`

| Attribute | Type | Role |
|-----------|------|------|
| `txn_id` | S | Hash key (PK) |
| `tuple_id` | S | Sort key (SK) |
| (same attributes as committed table) | | |

### Watches: `coordin8_space_watches`

| Attribute | Type | Role |
|-----------|------|------|
| `watch_id` | S | Hash key (PK) |
| `template` | M | HashMap<String, String> |
| `on` | S | "Appearance" or "Expiration" |
| `lease_id` | S | |
| `handback` | B | Binary blob |

GSI: `lease_id-index` (PK: `lease_id`) for `remove_watch_by_lease`

### DynamoDB Notes

- **4 tables** for Space (committed, uncommitted, txn_taken, watches). This is the design decision from Phase 3 prep.
- **Template matching on DynamoDB**: cannot use DynamoDB filter expressions for `contains:` and `starts_with:` operators on arbitrary attribute keys within a Map. Use a full table Scan + client-side filtering with `parse_template` / `matches` — same as InMemory. Acceptable for v1.
- **take_match atomicity**: `DeleteItem` with `ConditionExpression: "attribute_exists(tuple_id)"`. If condition fails, another process took it — retry with next match.
- **commit_txn**: query uncommitted by `txn_id`, `PutItem` each into committed, then `BatchWriteItem` delete from uncommitted. Delete txn_taken entries.
- **abort_txn**: query uncommitted by `txn_id`, delete all (discarded). Query txn_taken by `txn_id`, `PutItem` each back into committed, delete from txn_taken.
- **payload, handback**: `AttributeValue::B` (binary).
- **SpaceEventKind**: serialize as string "Appearance" or "Expiration".
- **input_tuple_id**: optional. Omit attribute when `None`.
- **AWS SDK error matching**: typed `SdkError::ServiceError` patterns only. Never string-match.
- **Pagination**: all Scan and Query operations must handle `LastEvaluatedKey`.
- **BatchWriteItem**: max 25 items per call. Paginate if needed.

## Dependencies

The DynamoDB SpaceStore needs `coordin8-registry` as a dependency for `matcher::{matches, parse_template}` — same as the InMemory version.

```toml
coordin8-registry = { workspace = true }
```

## How Space Wires Into the System

- Tuples have `lease_id`. Resource ID convention: `space:{tuple_id}`.
- Watches have `lease_id`. Resource ID convention: `space-watch:{watch_id}`.
- Lease expires → `SpaceManager::on_tuple_expired` or `on_watch_expired` from expiry cascade.
- SpaceManager wraps the store and adds broadcast channels for watch notifications.
- Space is also a 2PC participant — exposes `ParticipantService` for transaction commit/abort.

## File Locations

| File | Purpose |
|------|---------|
| `djinn/crates/coordin8-core/src/space.rs` | Trait + records + enums |
| `djinn/crates/coordin8-space/` | SpaceManager + watch broadcast + participant service |
| `djinn/crates/coordin8-registry/src/matcher.rs` | Template matching logic (shared) |
| `djinn/providers/local/src/space_store.rs` | InMemory reference (268 lines) |
| `djinn/providers/dynamo/src/space_store.rs` | DynamoDB implementation (to be created) |

## Test Expectations

1. **insert + get** — round-trip tuple with payload and attrs
2. **insert + remove** — remove returns the tuple
3. **remove_by_lease** — insert with lease, remove by lease
4. **find_match** — insert multiple, find by template
5. **find_match with txn** — committed + uncommitted visible within txn
6. **take_match** — take removes from committed store
7. **take_match under txn** — taken tuple tracked for restore-on-abort
8. **find_all_matches** — multiple matches returned
9. **commit_txn** — uncommitted flushed to committed, taken stays removed
10. **abort_txn** — uncommitted discarded, taken restored
11. **has_txn** — true when uncommitted or taken exist
12. **list_all** — all committed tuples returned
13. **create_watch + list_watches** — round-trip
14. **remove_watch** — remove, verify gone
15. **remove_watch_by_lease** — insert with lease, remove by lease
