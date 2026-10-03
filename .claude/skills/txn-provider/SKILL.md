---
name: txn-provider
description: Domain knowledge for the TxnStore trait and its InMemory/DynamoDB providers (2PC transaction records, participants, state transitions). Use when implementing or modifying TxnStore, TransactionMgr storage, the coordin8_txn table, or 2PC state-transition logic.
---

# txn-provider

Domain knowledge for implementing and modifying TxnStore providers. Load this skill when working on transaction-related storage backends.

## The Trait

```rust
// coordin8-core/src/txn.rs

#[derive(Debug, Clone, PartialEq)]
pub enum TransactionState {
    Active,
    Voting,
    Prepared,
    Committed,
    Aborted,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PrepareVote {
    Prepared,
    NotChanged,
    Aborted,
}

#[derive(Debug, Clone)]
pub struct ParticipantRecord {
    pub endpoint: String,
    pub crash_count: u64,
}

#[derive(Debug, Clone)]
pub struct TransactionRecord {
    pub txn_id: String,
    pub lease_id: String,
    pub state: TransactionState,
    pub participants: Vec<ParticipantRecord>,
    pub begun_at: DateTime<Utc>,
}

#[async_trait]
pub trait TxnStore: Send + Sync {
    async fn create(&self, record: TransactionRecord) -> Result<(), Error>;
    async fn get(&self, txn_id: &str) -> Result<Option<TransactionRecord>, Error>;
    async fn update_state(&self, txn_id: &str, state: TransactionState) -> Result<(), Error>;
    async fn add_participant(&self, txn_id: &str, participant: ParticipantRecord) -> Result<(), Error>;
    async fn remove(&self, txn_id: &str) -> Result<(), Error>;
    async fn list_all(&self) -> Result<Vec<TransactionRecord>, Error>;
}
```

## Behavioral Contract

The InMemory implementation (`providers/local/src/txn_store.rs`) is the reference.

### create
- Store the full TransactionRecord keyed by `txn_id`
- Overwrites if `txn_id` already exists (no error)

### get
- Return `Some(record)` or `None`. Never error on missing.

### update_state
- MUST return `Error::TransactionNotFound` if txn doesn't exist
- Update only the `state` field, preserve everything else
- **It is an unconditional overwrite of `state`** in both providers: the store does not validate the transition (e.g. it will happily set `Committed` -> `Active`, or overwrite a concurrent `Aborted`). The trait has no compare-and-set. Any caller that needs a guarded transition (lease-expiry abort racing a commit) must guard it above the store (TxnManager) or the trait needs a CAS variant implemented in BOTH providers (Dynamo: `ConditionExpression: #s = :expected`). Don't build a read-then-`update_state` state machine and call it atomic.

### add_participant
- MUST return `Error::TransactionNotFound` if txn doesn't exist
- Append the participant to the existing `participants` vec
- DynamoDB: use `list_append` in UpdateExpression

### remove
- Silent no-op if txn doesn't exist

### list_all
- Return all records. Handle DynamoDB scan pagination.

## Error Mapping

| Situation | Behavior |
|-----------|----------|
| Txn not found (get) | `Ok(None)` |
| Txn not found (update_state, add_participant) | `Error::TransactionNotFound(txn_id)` |
| Txn not found (remove) | `Ok(())` — silent |
| Backend failure | `Error::Storage(message)` |

## DynamoDB Table Schema

Table: `coordin8_txn` (`TXN_TABLE`; `DynamoTxnStore::new` / `with_table`; created by `init()` only when `COORDIN8_AUTO_CREATE_TABLES=true|1`, otherwise via `infra/dynamodb-tables.cfn.yml`)

| Attribute | Type | Role |
|-----------|------|------|
| `txn_id` | S | Hash key (PK) |
| `lease_id` | S | |
| `state` | S | TransactionState as string: "Active", "Voting", "Prepared", "Committed", "Aborted" |
| `participants` | L | List of maps: `[{"endpoint": S, "crash_count": N}, ...]` |
| `begun_at` | S | RFC 3339 timestamp |

No GSI needed — transactions are always looked up by `txn_id`.

### DynamoDB Notes

- `TransactionState` serialization: store as string, not number. Parse back with a match block.
- `participants`: DynamoDB List (L) of Maps (M). Each map has `endpoint` (S) and `crash_count` (N).
- `update_state`: use `UpdateItem` with `SET #state = :s` and `condition_expression("attribute_exists(txn_id)")`. On `ConditionalCheckFailedException`, return `Error::TransactionNotFound`.
- `add_participant`: use `UpdateItem` with `SET participants = list_append(participants, :p)` and `condition_expression("attribute_exists(txn_id)")`.
- **AWS SDK error matching**: always use typed `SdkError::ServiceError` + `is_conditional_check_failed_exception()`. Never string-match.
- `list_all`: paginated scan with `LastEvaluatedKey`.

## How Transactions Wire Into the System

- Each transaction has a `lease_id` granted by TransactionMgr's OWN embedded `LeaseManager` (namespace `txn`; Dynamo lease table `coordin8_leases_txn`; port 9004). The resource_id convention is `txn:{txn_id}`.
- Lease reclaimed → `TxnManager::abort_expired(txn_id)` is called from the cascade in `coordin8-djinn/src/services.rs` (`run_all` / `run_txn_on_listener`).
- The TxnManager orchestrates 2PC: begin → enlist participants → prepare → commit/abort.
- Participants expose `ParticipantService` gRPC — the TxnManager calls them back during prepare/commit/abort. Space is one (auto-enlists via the `TxnEnlister` trait in `coordin8-core`: `LocalTxnEnlister` in bundled mode, `RemoteTxnEnlister` discovering TxnMgr lazily through Registry in split mode).
- `participants` is read-modify-append: concurrent `add_participant` on InMemory is a `push` under the map lock, on Dynamo an atomic `list_append`. Both are safe; duplicates are NOT de-duplicated by either.

## File Locations

| File | Purpose |
|------|---------|
| `djinn/crates/coordin8-core/src/txn.rs` | Trait + records + enums |
| `djinn/crates/coordin8-txn/` | TxnManager + 2PC orchestration |
| `djinn/providers/local/src/txn_store.rs` | InMemory reference |
| `djinn/providers/dynamo/src/txn_store.rs` | DynamoDB implementation |
| `djinn/crates/coordin8-djinn/src/services.rs` | `txn_store_from_env()` + expiry cascade |

## Test Expectations

1. **create + get** — round-trip, verify all fields including empty participants
2. **update_state** — change Active → Committed, verify
3. **update_state non-existent** — returns TransactionNotFound
4. **add_participant** — add participant, verify in get
5. **add_participant non-existent** — returns TransactionNotFound
6. **remove** — remove, verify get returns None
7. **remove non-existent** — silent, no error
8. **list_all** — multiple txns, verify all returned

Dynamo tests are `#[ignore]` (need MiniStack) and never run in CI: `cd djinn && cargo test -p coordin8-provider-dynamo txn_store -- --ignored`.
