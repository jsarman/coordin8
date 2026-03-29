# Next Session Prep — Post EventMgr + TransactionMgr

## Where We Left Off

EventMgr and TransactionMgr are fully implemented and tested in Rust. Both have Go proto stubs and live demos. Everything is committed and clean.

## What's Next

### Option A — SDK clients for EventMgr + TransactionMgr (Go first)
Follow the same pattern as `sdks/go/coordin8/lease.go` and `registry.go`.

**EventClient** (`sdks/go/coordin8/event.go`):
- `Subscribe(source, template, delivery, ttl, handback)` → `EventRegistration`
- `Emit(source, eventType, attrs, payload)` → error
- `Receive(registrationId)` → `chan Event` (goroutine wrapping the stream)
- `RenewSubscription(registrationId, ttl)` → error
- `CancelSubscription(registrationId)` → error

**TransactionClient** (`sdks/go/coordin8/txn.go`):
- `Begin(ttlSeconds)` → `Transaction`
- `Enlist(txnId, participantEndpoint)` → error
- `Commit(txnId)` → error
- `Abort(txnId)` → error
- `GetState(txnId)` → `TransactionState`

### Option B — Space
The big one. Unlocks the core coordination model. See `coordin8-design-napkin.md` for the full spec.

Primitives: `out(tuple, txn, ttl)`, `take(template, txn, wait)`, `read(template, txn, wait)`, `watch(template, on)`

New crate: `coordin8-space`. New proto: `proto/coordin8/space.proto`. New store: `InMemorySpaceStore`.

### Option C — SDK parity gaps (Java/Node)
- Java: `keepAlive` (LeaseMgr), `watch` (Registry), `register` (Registry)
- Java/Node: ServiceDiscovery watch-based refresh (cache never refreshes after initial populate)
- Java/Node: EventMgr + TransactionMgr clients

## Reference
- Completed work: `session-1-complete.md`
- Full PRD: `.claude/plans/PRD.md`
- Design vision: `coordin8-design-napkin.md`
