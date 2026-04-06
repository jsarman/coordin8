# coordin8-space

Space — distributed reactive tuple store. Inspired by JavaSpaces: write tuples with `out`, read without removing with `read`, atomically remove with `take`, subscribe to changes with `watch`.

## Where It Fits

Layer 2c (port `9006`), peer with Registry and EventMgr. Tuples and watches both hold leases through [`coordin8-lease`](../coordin8-lease/README.md):

- `space:<lease_id>` → an outstanding tuple; expiry removes it and broadcasts a tuple-expired event
- `space-watch:<lease_id>` → a live watch; expiry tears it down

The Space also acts as a transaction participant — `SpaceParticipantService` is registered as a `ParticipantServiceServer` on the same port. Uncommitted writes are invisible to other clients, and tuples taken under a transaction are restored on abort.

## Operations

| Op      | Effect |
|---------|--------|
| `out`   | Write a tuple. Optional transaction context. |
| `read`  | Match-and-return without removing. |
| `take`  | Match-and-remove atomically. |
| `watch` | Stream `WRITTEN` / `TAKEN` / `EXPIRED` events for templates. |

All matching uses the same operator set as the Registry (`contains:`, `starts_with:`, exact, `Any`).

## Layout

```
src/
  lib.rs           re-exports SpaceManager, SpaceServiceImpl, SpaceParticipantService
  manager.rs       state — tuples, uncommitted writes, taken-under-txn, watches
  service.rs       tonic server impl for SpaceService
  participant.rs   tonic server impl for ParticipantService (2PC hooks)
tests/             integration tests for transactional isolation + watch delivery
```

## Build / Test

```bash
cargo build -p coordin8-space
cargo test  -p coordin8-space
```

## Notes

- The DynamoDB provider keeps four tables for the Space: `coordin8_space`, `coordin8_space_uncommitted`, `coordin8_space_txn_taken`, `coordin8_space_watches`
- Two broadcast channels feed the watch system: one for live tuple writes, one for expirations
- `on_tuple_expired` and `on_watch_expired` are the hooks the Djinn binary invokes from the lease-expiry task
