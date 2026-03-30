# Session 1 Complete — Space

**Outcome:** v1 done. Builds clean. All 12 tests pass. Djinn boots with Space on :9006.

---

## What Got Done

### Proto
- Created `proto/coordin8/space.proto` — SpaceService with 6 RPCs: Out, Read, Take, Watch, RenewTuple, CancelTuple
- Messages: Tuple, Provenance, OutRequest/Response, ReadRequest/Response, TakeRequest/Response, WatchRequest, SpaceEvent, SpaceEventType enum, RenewTupleRequest, CancelTupleRequest

### coordin8-core
- Added `space.rs` — `TupleRecord`, `SpaceWatchRecord`, `SpaceEventKind`, `SpaceStore` trait (13 methods)
- `lib.rs` — re-exports all space types
- `error.rs` — added `TupleNotFound(String)` and `WatchNotFound(String)` variants

### providers/local
- Added `space_store.rs` — `InMemorySpaceStore`
  - `DashMap<String, TupleRecord>` + `DashMap<String, String>` lease_index for reaper lookups
  - `DashMap<String, SpaceWatchRecord>` + watch_lease_index
  - `take_match` is race-safe: scan → remove → if None (raced), loop and try next
- Added `coordin8-registry` dependency for matcher
- `lib.rs` — exports `InMemorySpaceStore`

### coordin8-space (new crate)
- `manager.rs` — `SpaceManager`
  - `out()` → generate UUID, grant lease, insert, broadcast
  - `read()` → find_match, if wait then subscribe to broadcast + tokio::select with timeout
  - `take()` → take_match, if wait then subscribe + re-query store on each notification (race-safe)
  - `watch()` → create watch record, grant lease
  - `on_tuple_expired()` → remove by lease, broadcast on expiry_tx
  - `on_watch_expired()` → remove watch by lease
  - `cancel_tuple()`, `renew_tuple()` → store remove/get + lease cancel/renew
- `service.rs` — `SpaceServiceImpl` (tonic)
  - All 6 RPCs implemented
  - `Watch` uses BroadcastStream + spawned filter task, same pattern as EventMgr's Receive
  - `Out` returns full Tuple with Provenance + Lease
  - `Read`/`Take` return optional Tuple (absent = no match)

### coordin8-proto
- `build.rs` — added `space.proto` to compile list

### djinn Cargo workspace
- Added `coordin8-space` to workspace members and deps
- `coordin8-djinn/Cargo.toml` — added `coordin8-space` dep

### main.rs
- Layer 2c boot: InMemorySpaceStore + broadcast channels + SpaceManager
- Lease expiry listener: dispatches `space:` prefixed leases to `on_tuple_expired`, `space-watch:` to `on_watch_expired`
- `SpaceServiceServer` added to `tokio::try_join!` on port 9006
- Boot log shows `✓ Space: ready` and `✓ Space: listening on 0.0.0.0:9006`

### docker-compose.yml
- Exposed `9006:9006`
- Added `coordin8_space=debug` to `RUST_LOG`

### Makefile
- Added `space.proto` to Go proto generation target

### Tests — 12 passing
- out_and_read, out_and_take, read/take_nonblocking_empty
- take_blocking_unblocks_on_out, read_blocking_timeout
- template_matching (exact, contains, starts_with, wildcard, miss)
- tuple_lease_expiry (via reaper), watch_appearance
- take_race_one_winner, cancel_tuple_removes, provenance_tracking

---

## Surprises / Decisions Made

See `decisions.md` for full detail. Short version:

- Boot layer is 2c (peer with Registry + EventMgr), not 5 — Space only depends on LeaseMgr at runtime
- `take_match` race safety required a retry loop — DashMap isn't transactional, so scan-then-remove can race with another taker
- Blocking take re-queries store on broadcast notification — can't trust the broadcast event alone because another taker may have claimed it between broadcast and your attempt
- `providers/local` now depends on `coordin8-registry` for the matcher — utility dep, not a service dep

---

## State of the Codebase

- `cargo build` — clean, zero warnings
- `cargo clippy --all -- -D warnings` — clean
- `cargo test --all` — 30 tests pass (12 space + 18 existing)
- No regressions in any existing crate

---

## What's Next

1. **Go SDK SpaceClient** — follow LeaseClient/EventClient pattern
2. **Java + Node SDK** — after Go lands
3. **CLI: spaces commands** — list, read, out, watch
4. **Space example** — similar to market-watch but tuple-based coordination
5. **v2: Transaction support** — txn_id on operations, Space as 2PC participant
6. **Higher-order patterns** — Lens, Reflex, Sentry (now unblocked)
