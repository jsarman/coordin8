# Phase 3: Cloud Topology — Session 1 Prep

**Goal:** Build a DynamoDB-backed provider (`coordin8-provider-dynamo`) and validate it against LocalStack. Same traits, same managers, different backing store.

## Why LocalStack First

- No AWS account needed during dev
- Same DynamoDB API, runs locally in Docker
- Already stubbed in `docker-compose.yml` (commented out)
- Validates the provider abstraction end-to-end before touching real AWS
- Tests run in CI without credentials

## What Needs to Happen

### 1. Activate LocalStack in Docker Compose

Uncomment the `localstack` service. Services needed: `dynamodb`.

SQS and EventBridge come later — start with the storage layer (DynamoDB) since all five store traits map to it.

### 2. Create `providers/dynamo/` Crate

```
djinn/providers/dynamo/
  Cargo.toml
  src/
    lib.rs
    lease_store.rs      — DynamoLeaseStore implements LeaseStore
    registry_store.rs   — DynamoRegistryStore implements RegistryStore
    space_store.rs      — DynamoSpaceStore implements SpaceStore
    txn_store.rs        — DynamoTxnStore implements TxnStore
    event_store.rs      — DynamoEventStore implements EventStore
    table.rs            — Table creation helpers (ensure_table)
    client.rs           — Shared DynamoDB client config
```

Add to workspace `Cargo.toml` members + dependencies.

### 3. DynamoDB Table Design

One table per store, single-table design where it makes sense.

#### `coordin8_leases`
| PK | SK | Attributes |
|----|----|-|
| `lease_id` | — | `resource_id`, `granted_at`, `expires_at`, `ttl_seconds`, `ttl` (epoch for DynamoDB TTL) |

- GSI: `resource_id-index` (resource_id → lease_id) for `get_by_resource`
- DynamoDB TTL on `ttl` field — this IS the reaper for production
- For LocalStack: still need our reaper since LocalStack TTL is approximate

#### `coordin8_registry`
| PK | SK | Attributes |
|----|----|-|
| `capability_id` | — | `interface`, `attrs` (map), `transport_type`, `transport_config` (map), `lease_id` |

- GSI: `lease_id-index` (lease_id → capability_id) for `remove_by_lease`

#### `coordin8_space`
| PK | SK | Attributes |
|----|----|-|
| `tuple_id` | — | `attrs` (map), `payload` (B), `lease_id`, `written_by`, `written_at`, `input_tuple_id` |

- GSI: `lease_id-index` for `remove_by_lease`
- Uncommitted buffer: separate table `coordin8_space_uncommitted` keyed by `txn_id#tuple_id`
- Txn-taken buffer: separate table `coordin8_space_txn_taken` keyed by `txn_id#tuple_id`
- Take atomicity: `DeleteItem` with `ConditionExpression: "attribute_exists(tuple_id)"` — exactly one winner

#### `coordin8_txn`
| PK | SK | Attributes |
|----|----|-|
| `txn_id` | — | `lease_id`, `state`, `participants` (L of maps), `begun_at` |

#### `coordin8_events`
| PK | SK | Attributes |
|----|----|-|
| `registration_id` | — | `template` (map), `event_type`, `source`, `delivery_mode`, `lease_id`, `seq_number`, `handback` |

Mailbox: `coordin8_event_mailbox` with PK=`registration_id`, SK=`seq_number`

#### Watches (Space)
| PK | SK | Attributes |
|----|----|-|
| `watch_id` | — | `template` (map), `on`, `lease_id`, `handback` |

- GSI: `lease_id-index` for `remove_watch_by_lease`

### 4. Implementation Order

Start with LeaseStore — it's the bedrock, simplest trait, and validates the DynamoDB client setup.

1. **DynamoLeaseStore** — 7 methods. Table creation. GSI for resource_id lookup. `list_expired` scans for `expires_at < now`.
2. **DynamoRegistryStore** — 6 methods. GSI for lease_id lookup.
3. **DynamoTxnStore** — 6 methods. Participants stored as list attribute.
4. **DynamoEventStore** — 7 methods. Mailbox as separate table with SortKey for ordering.
5. **DynamoSpaceStore** — 15 methods (biggest). Uncommitted/txn_taken as separate tables. Conditional deletes for atomic take.

### 5. Testing Strategy

Each store gets integration tests that:
- Create tables in LocalStack on test setup
- Run the same logical tests as the InMemory provider
- Clean up or use unique table names per test run

Use `aws-sdk-dynamodb` crate with custom endpoint (`http://localhost:4566`).

### 6. Wiring: Provider Selection in main.rs

Environment variable: `COORDIN8_PROVIDER=local|dynamo`

When `dynamo`:
- Read `DYNAMODB_ENDPOINT` (defaults to `http://localhost:4566` for LocalStack, omit for real AWS)
- Create shared DynamoDB client
- Ensure all tables exist (create if missing)
- Instantiate Dynamo stores instead of InMemory stores
- Everything else (managers, services, gRPC servers) stays identical

```rust
// Pseudocode
let provider = std::env::var("COORDIN8_PROVIDER").unwrap_or("local".into());

match provider.as_str() {
    "dynamo" => {
        let client = make_dynamo_client().await;
        ensure_tables(&client).await;
        let lease_store = Arc::new(DynamoLeaseStore::new(client.clone()));
        // ...
    }
    _ => {
        let lease_store = Arc::new(InMemoryLeaseStore::new());
        // ...
    }
}
```

### 7. Dependencies to Add

```toml
# In workspace Cargo.toml
aws-config    = "1"
aws-sdk-dynamodb = "1"

# In providers/dynamo/Cargo.toml
[dependencies]
coordin8-core     = { workspace = true }
coordin8-registry = { workspace = true }   # for matcher (template ops on scan)
aws-sdk-dynamodb  = { workspace = true }
aws-config        = { workspace = true }
async-trait       = { workspace = true }
tokio             = { workspace = true }
uuid              = { workspace = true }
chrono            = { workspace = true }
tracing           = { workspace = true }
serde_json        = { workspace = true }
```

## Session Plan

Token-conscious — checkpoint after each milestone, ask to continue.

1. Activate LocalStack in docker-compose, verify it starts → **checkpoint**
2. Create the `providers/dynamo/` crate skeleton, shared client + table helpers → **checkpoint**
3. Implement `DynamoLeaseStore` + test against LocalStack → **checkpoint**
4. Implement `DynamoRegistryStore` + test → **checkpoint**
5. Implement `DynamoTxnStore` + test → **checkpoint**
6. Implement `DynamoEventStore` + test → **checkpoint**
7. Implement `DynamoSpaceStore` + test → **checkpoint**
8. Wire provider selection in main.rs → **checkpoint**
9. Full stack test with `COORDIN8_PROVIDER=dynamo` in Docker → **checkpoint**

Each step: implement, test, commit, ask to continue. No hog wild.

## Risks

- **LocalStack DynamoDB fidelity** — conditional expressions, GSI consistency, and TTL behavior may differ from real DynamoDB. Test against real AWS before shipping.
- **Template matching on DynamoDB** — `contains:` and `starts_with:` operators don't map cleanly to DynamoDB filter expressions for arbitrary attribute keys. May need client-side filtering on scan results (like InMemory does today). Acceptable for v1.
- **Scan performance** — `find_match`, `take_match`, `find_all_matches` all do full table scans with template matching. Fine for small spaces, needs GSI strategy for scale. Park for later.
- **Uncommitted buffer tables** — three tables for Space (committed, uncommitted, txn_taken) feels heavy. Could use a single table with a `status` attribute instead. Decide during implementation.

## Files to Touch

- `docker-compose.yml` — uncomment LocalStack
- `djinn/Cargo.toml` — add workspace member + dependencies
- `djinn/providers/dynamo/` — new crate (all files)
- `djinn/crates/coordin8-djinn/Cargo.toml` — add dynamo provider dependency
- `djinn/crates/coordin8-djinn/src/main.rs` — provider selection logic
