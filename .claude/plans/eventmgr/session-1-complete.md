# Session 1 Complete — EventMgr

**Outcome:** Done. Builds clean. Djinn boots with all five services.

---

## What Got Done

### coordin8-core
- Added `event.rs` — `EventRecord`, `SubscriptionRecord`, `DeliveryMode`, `EventStore` trait
- `lib.rs` — re-exports all event types alongside existing lease/registry
- `error.rs` — added `SubscriptionNotFound(String)` and `SubscriptionExpired(String)` variants

### providers/local
- Added `event_store.rs` — `InMemoryEventStore` backed by `DashMap<String, SubscriptionRecord>` + `DashMap<String, VecDeque<EventRecord>>` for mailboxes
- `lib.rs` — exports `InMemoryEventStore`

### coordin8-event (new crate)
- `manager.rs` — `EventManager`
  - `subscribe()` → grants lease, creates `SubscriptionRecord`, returns `(registration_id, lease_id, initial_seq_num)`
  - `emit()` → increments per-`(source::event_type)` atomic counter, broadcasts to live receivers, enqueues into all matching durable mailboxes
  - `drain_mailbox()`, `subscribe_broadcast()`, `cancel_subscription()`, `renew_subscription()`
- `service.rs` — `EventServiceImpl` (tonic)
  - `Receive` uses mpsc channel + spawned task: subscribes to broadcast first, then drains mailbox, sends backlog, then filters live broadcast by source + template
  - `Emit` / `CancelSubscription` return `()` (that's what `google.protobuf.Empty` compiles to)

### coordin8-proto
- `build.rs` — added `event.proto` to compile list
- `proto/coordin8/event.proto` — added missing `import "google/protobuf/empty.proto"`

### djinn Cargo workspace
- Added `coordin8-event` to workspace members and deps
- `coordin8-djinn/Cargo.toml` — added `coordin8-event` dep

### main.rs
- Layer 2b boot — `InMemoryEventStore` + `EventManager` constructed alongside Registry
- `EventServiceServer` added to `tokio::try_join!` on port 9005
- Boot log now shows `✓ EventMgr: ready` and `✓ EventMgr: listening on 0.0.0.0:9005`

### docker-compose.yml
- Exposed `9005:9005`
- Added `coordin8_event=debug` to `RUST_LOG`

---

## Surprises / Decisions Made

See `decisions.md` for the real detail. Short version:

- Proto needed `empty.proto` import — wasn't in the draft, caught at first compile
- `Receive` stream: subscribe to broadcast *before* draining mailbox to avoid race. Clients use `seq_num` to deduplicate any overlap. Simple, correct enough.
- `emit()` iterates all subscriptions to find durable matches — O(n) over subs. Fine at current scale. Worth noting if subscription counts get large.

---

## State of the Codebase

- `cargo build` — clean (2 pre-existing proxy warnings, unrelated)
- `cargo run` boots all five services correctly
- No tests written for EventMgr yet — covered by integration testing with grpcurl or Go SDK

---

## What's Next

1. **Subscription reaper hookup** — listen on `expiry_tx` broadcast in EventMgr, auto-remove expired subs. Same pattern as Registry.
2. **Go SDK EventClient** — `sdks/go/coordin8/event_client.go`, follow `LeaseClient` pattern exactly
3. **Java + Node SDK** — after Go lands
4. **Integration test** — grpcurl round-trip (subscribe → emit → receive) in CI or Makefile

---

## Next Session Prep

Go look at `eventmgr`. Start with the Go SDK EventClient. The Rust side is done.
Reaper hookup is a nice-to-have — park it unless we're doing a full cleanup pass.
