# event-provider

Domain knowledge for implementing and modifying EventStore providers. Load this skill when working on event subscription and mailbox storage backends.

## The Trait

```rust
// coordin8-core/src/event.rs

#[derive(Debug, Clone)]
pub struct EventRecord {
    pub event_id: String,
    pub source: String,
    pub event_type: String,
    pub seq_num: u64,
    pub attrs: HashMap<String, String>,
    pub payload: Vec<u8>,
    pub emitted_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct SubscriptionRecord {
    pub registration_id: String,
    pub source: String,
    pub template: HashMap<String, String>,
    pub delivery: DeliveryMode,
    pub lease_id: String,
    pub handback: Vec<u8>,
    pub initial_seq_num: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DeliveryMode {
    Durable,
    BestEffort,
}

#[async_trait]
pub trait EventStore: Send + Sync {
    async fn create_subscription(&self, sub: SubscriptionRecord) -> Result<(), Error>;
    async fn get_subscription(&self, registration_id: &str) -> Result<Option<SubscriptionRecord>, Error>;
    async fn remove_subscription(&self, registration_id: &str) -> Result<(), Error>;
    async fn remove_by_lease(&self, lease_id: &str) -> Result<Option<SubscriptionRecord>, Error>;
    async fn list_subscriptions(&self) -> Result<Vec<SubscriptionRecord>, Error>;
    async fn enqueue(&self, registration_id: &str, event: EventRecord) -> Result<(), Error>;
    async fn dequeue(&self, registration_id: &str) -> Result<Vec<EventRecord>, Error>;
}
```

## Behavioral Contract

The InMemory implementation (`providers/local/src/event_store.rs`) is the reference.

### create_subscription
- Store the subscription keyed by `registration_id`
- Initialize an empty mailbox for this subscription
- Overwrites if `registration_id` already exists

### get_subscription
- Return `Some(sub)` or `None`. Never error on missing.

### remove_subscription
- Remove both the subscription AND its mailbox
- Silent no-op if not found

### remove_by_lease
- Find the subscription with matching `lease_id`, remove it and its mailbox
- Return `Some(sub)` if found, `None` if not
- This is the expiry path

### list_subscriptions
- Return all subscriptions

### enqueue
- Append event to the subscription's mailbox
- MUST return `Error::SubscriptionNotFound` if no mailbox exists

### dequeue
- Drain and return ALL queued events for the subscription
- After dequeue, the mailbox should be empty
- MUST return `Error::SubscriptionNotFound` if no mailbox exists

## Error Mapping

| Situation | Behavior |
|-----------|----------|
| Subscription not found (get) | `Ok(None)` |
| Subscription not found (remove) | `Ok(())` — silent |
| Subscription not found (remove_by_lease) | `Ok(None)` |
| Subscription not found (enqueue, dequeue) | `Error::SubscriptionNotFound(id)` |
| Backend failure | `Error::Storage(message)` |

## DynamoDB Table Schema

### Subscriptions table: `coordin8_events`

| Attribute | Type | Role |
|-----------|------|------|
| `registration_id` | S | Hash key (PK) |
| `source` | S | Event source filter |
| `template` | M | HashMap<String, String> as DynamoDB Map |
| `delivery_mode` | S | "Durable" or "BestEffort" |
| `lease_id` | S | |
| `handback` | B | Binary blob |
| `initial_seq_num` | N | Starting sequence number |

GSI: `lease_id-index` (PK: `lease_id`) for `remove_by_lease`

### Mailbox table: `coordin8_event_mailbox`

| Attribute | Type | Role |
|-----------|------|------|
| `registration_id` | S | Hash key (PK) — groups events by subscription |
| `seq_num` | N | Sort key (SK) — ordering within subscription |
| `event_id` | S | |
| `source` | S | |
| `event_type` | S | |
| `attrs` | M | HashMap<String, String> |
| `payload` | B | Binary blob |
| `emitted_at` | S | RFC 3339 timestamp |

### DynamoDB Notes

- Two tables — subscriptions and mailbox are separate (different access patterns)
- `enqueue`: `PutItem` into mailbox table. The `seq_num` is the sort key — events are naturally ordered.
- `dequeue`: `Query` mailbox by `registration_id` (all items), collect results, then `BatchWriteItem` to delete them all. Handle pagination on both the query and the batch delete (max 25 items per batch).
- `remove_subscription`: delete from subscriptions table AND delete all mailbox items for that `registration_id`
- `remove_by_lease`: query the `lease_id-index` GSI to find the `registration_id`, then remove subscription + mailbox
- `handback` and `payload`: store as `AttributeValue::B` (binary). Use `Blob::new(vec)` from the AWS SDK.
- `DeliveryMode`: serialize as string "Durable" or "BestEffort"
- **AWS SDK error matching**: always use typed `SdkError::ServiceError` patterns. Never string-match.
- Paginate `list_subscriptions` with `LastEvaluatedKey`.

## How Events Wire Into the System

- Each subscription has a `lease_id`. The resource_id convention is `event:{registration_id}`.
- Lease expires → `EventManager::unsubscribe_by_lease(lease_id)` called from expiry cascade.
- The EventManager wraps the store and adds broadcast delivery for live streams.
- Durable subscriptions get mailboxed events; BestEffort subscriptions only get live broadcast.

## File Locations

| File | Purpose |
|------|---------|
| `djinn/crates/coordin8-core/src/event.rs` | Trait + records + enums |
| `djinn/crates/coordin8-event/` | EventManager + broadcast delivery |
| `djinn/providers/local/src/event_store.rs` | InMemory reference |
| `djinn/providers/dynamo/src/event_store.rs` | DynamoDB implementation (to be created) |

## Test Expectations

1. **create_subscription + get** — round-trip, verify all fields
2. **remove_subscription** — remove, verify get returns None
3. **remove_subscription non-existent** — silent, no error
4. **remove_by_lease** — insert with lease, remove by lease, verify gone
5. **remove_by_lease non-existent** — returns None
6. **list_subscriptions** — multiple subs, verify all returned
7. **enqueue + dequeue** — enqueue 3 events, dequeue all, verify ordered by seq_num
8. **dequeue empties mailbox** — dequeue twice, second returns empty vec
9. **enqueue non-existent subscription** — returns SubscriptionNotFound
10. **dequeue non-existent subscription** — returns SubscriptionNotFound
11. **remove_subscription cleans mailbox** — enqueue events, remove sub, verify mailbox gone
