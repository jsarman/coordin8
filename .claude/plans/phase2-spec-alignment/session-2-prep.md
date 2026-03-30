# Phase 2: Spec Alignment — Session 2 Prep

**Goal:** Implement transactional isolation on Space operations — the final major Jini spec gap.

## The Scenario That Should Work

```
1. Begin transaction (txn_id)
2. Write tuple under txn_id → tuple invisible to non-transactional reads
3. Read without txn_id → nothing (isolation)
4. Take without txn_id → nothing (isolation)
5. Read WITH txn_id → sees the tuple (transaction-scoped visibility)
6. Take WITH txn_id → gets the tuple
7. Commit → tuple removed (take already claimed it)

Alt flow:
1. Begin → Write under txn → Abort → tuple discarded, never visible
```

## What Needs to Happen

### 1. Uncommitted Write Buffer (SpaceStore)
- New store method: `insert_uncommitted(txn_id, record)` — holds tuples in a shadow buffer keyed by txn_id
- `find_match` / `take_match` need a `txn_id: Option<&str>` parameter:
  - `None` → only search committed tuples
  - `Some(id)` → search committed tuples AND that txn's uncommitted buffer
- New: `commit_txn(txn_id)` — flush all uncommitted tuples for that txn into the visible store
- New: `abort_txn(txn_id)` — discard all uncommitted tuples for that txn

### 2. SpaceManager Changes
- `write()` checks `txn_id` — if present, insert into uncommitted buffer instead of visible store, don't broadcast yet
- `read()` / `take()` pass `txn_id` through to store
- New: `commit_space_txn(txn_id)` — flush + broadcast all newly visible tuples
- New: `abort_space_txn(txn_id)` — discard + cancel leases on uncommitted tuples

### 3. Space as a 2PC Participant
- SpaceManager needs to implement `ParticipantService` (or a wrapper does)
- On `write(txn_id=X)`, the Space auto-enlists itself as a participant in txn X
- `Prepare` → return PREPARED (tuples are buffered, ready to flush)
- `Commit` → call `commit_space_txn(txn_id)`
- `Abort` → call `abort_space_txn(txn_id)`
- The Space needs a gRPC server for `ParticipantService` — could run on the same port (9006) or a separate one

### 4. Proto Changes
- None needed — `txn_id` fields already reserved on Write/Read/Take/Notify/Contents
- Space's ParticipantService endpoint needs to be known to TxnMgr — the Space auto-enlists via `TransactionService/Enlist` with its own endpoint

### 5. Key Design Decision: Where Does the Participant Server Run?
Option A: Space service hosts ParticipantService on a separate port (e.g., 9007)
Option B: Space service hosts ParticipantService on the same port (9006) via tonic's `add_service` — **preferred**, cleaner

### 6. InMemorySpaceStore Changes
- Add `uncommitted: DashMap<String, Vec<TupleRecord>>` keyed by txn_id
- Modify `find_match` / `take_match` signatures to accept optional txn_id
- Add `commit_txn` / `abort_txn` methods

## Test Plan
1. Write under txn → read without txn → None
2. Write under txn → read with txn → found
3. Write under txn → commit → read without txn → found
4. Write under txn → abort → read without txn → None
5. Take under txn → commit → gone
6. Multiple txns → isolation between them
7. Lease expiry on txn → auto-abort → uncommitted tuples discarded

## Files to Touch
- `proto/coordin8/space.proto` — no changes needed
- `djinn/crates/coordin8-core/src/space.rs` — SpaceStore trait gains txn-aware methods
- `djinn/providers/local/src/space_store.rs` — uncommitted buffer + commit/abort
- `djinn/crates/coordin8-space/src/manager.rs` — txn-aware write/read/take + participant logic
- `djinn/crates/coordin8-space/src/service.rs` — pass txn_id through from proto
- `djinn/crates/coordin8-space/src/participant.rs` — new: ParticipantService impl
- `djinn/crates/coordin8-djinn/src/main.rs` — wire ParticipantService on Space port
- `djinn/crates/coordin8-space/tests/demo.rs` — transactional isolation tests

## Risks
- **Deadlock on take:** If two txns both try to take the same tuple, one blocks forever. Jini spec says take under txn should lock the entry — need a lock table or conflict detection.
- **Broadcast timing:** Tuples should only broadcast (wake blocked readers) on commit, not on write. Currently broadcast is in `write()`.
- **Transaction timeout:** If a txn lease expires while Space holds uncommitted tuples, the abort cascade must clean them up. Already have the cascade wired — just need `abort_space_txn` to be called.
