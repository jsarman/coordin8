# Session 1 Prep — EventMgr

> (Written after the fact. It's fine. It was in my head.)

---

## Where We Were

Djinn had three live services: LeaseMgr (9001), Registry (9002), Proxy (9003). All wired, working, tested end-to-end with the greeter example.

`event.proto` was already drafted and sitting in `proto/coordin8/`. Never compiled, never wired. Just vibes on disk.

## The Goal This Session

Full Rust implementation of EventMgr from soup to nuts:

1. Core types + trait (`EventRecord`, `SubscriptionRecord`, `EventStore`) in `coordin8-core`
2. `InMemoryEventStore` in `providers/local`
3. New `coordin8-event` crate — `EventManager` + `EventServiceImpl`
4. Wire into Djinn at Layer 2b, port 9005
5. Build green, Djinn boots with `✓ EventMgr: listening on 0.0.0.0:9005`

## Known Gotchas Going In

- `event.proto` was missing `import "google/protobuf/empty.proto"` — needed for the `Emit` and `CancelSubscription` returns
- `google.protobuf.Empty` compiles to `()` in tonic-generated Rust — confirm before writing service impl
- `coordin8_registry::matcher` is already public — reuse it directly, don't reinvent
- Boot order: EventMgr is a peer to Registry, not downstream. Both Layer 2. No dependency between them.
- `DashMap<String, AtomicU64>` for seq counters — valid, but hold refs only in sync context, never across awaits

## Out of Scope This Session

- Go SDK EventClient
- Subscription reaper (lease expiry hookup)
- Persistent event log / replay
