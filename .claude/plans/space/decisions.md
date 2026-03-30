# Space — Architecture Decisions

## 1. Transactions deferred to v2

**Decision:** `txn_id` is intentionally omitted from all v1 messages.

**Why:** Transaction support adds cross-cutting complexity — visibility isolation (uncommitted tuples invisible to other readers), Space as a 2PC participant, rollback semantics for partial takes. The Space is fully useful without it. JavaSpaces spec allows non-transactional operations and that's the common path.

**Impact:** v2 will add optional `txn_id` fields to Out/Read/Take requests and enlist Space as a TransactionMgr participant.

---

## 2. take_match is race-safe via retry loop

**Decision:** `InMemorySpaceStore::take_match` scans for a match, then calls `DashMap::remove()`. If remove returns None (another taker raced and won), it loops and tries the next match.

**Why:** DashMap is not transactional. Two concurrent takers can both find the same candidate. The `remove()` call is atomic — only the first one gets the value back. The loser continues scanning. This guarantees exactly one winner without locks.

---

## 3. Blocking take re-queries store, not broadcast

**Decision:** When a blocking take receives a broadcast notification, it calls `store.take_match()` rather than trying to claim the broadcast event directly.

**Why:** The broadcast is informational — it tells you *something* matching your template appeared. But between the broadcast and your claim attempt, another taker may have already taken it. Re-querying the store atomically is the only safe path. Same reasoning as JavaSpaces: the notification wakes you up, the store operation is the real claim.

---

## 4. Two broadcast channels (tuple_tx + expiry_tx)

**Decision:** Separate broadcast channels for appearance and expiration events.

**Why:** Watch subscriptions filter by event type (APPEARANCE or EXPIRATION). Mixing them into one channel would require every watch stream to filter out events it doesn't care about. Separate channels mean the service layer subscribes to exactly the right one.

---

## 5. Lease prefix convention: space: and space-watch:

**Decision:** Tuple leases use `space:{tuple_id}`, watch leases use `space-watch:{watch_id}` as the resource_id.

**Why:** The lease expiry listener in main.rs dispatches based on prefix. This is the same pattern as EventMgr (`event:{registration_id}`). It keeps the expiry listener simple — one `starts_with` check, no registry lookups.

---

## 6. Boot layer 2c, not layer 5

**Decision:** Space boots as Layer 2c (peer with Registry + EventMgr), not after TransactionMgr.

**Why:** Space depends only on LeaseMgr and Registry's matcher (compile-time dependency, not runtime). It has no dependency on Proxy or TransactionMgr. Placing it at Layer 2c reflects the actual dependency graph. The plan comment in main.rs previously said "Layer 5" — updated to 2c.

---

## 7. InMemorySpaceStore needs coordin8-registry dependency

**Decision:** `providers/local/Cargo.toml` now depends on `coordin8-registry` for the matcher.

**Why:** `find_match` and `take_match` need `parse_template` and `matches` from the matcher module. This is a compile-time dependency on a utility module, not a runtime service dependency. The alternative (duplicating the matcher) was worse.

---

## 8. Proto uses flat SpaceEventType enum

**Decision:** `SpaceEventType { APPEARANCE = 0; EXPIRATION = 1; }` — not nested inside another message.

**Why:** Protobuf enums in the same package share a namespace. `APPEARANCE` and `EXPIRATION` are unique enough — no collision risk with other enums in coordin8 package.
