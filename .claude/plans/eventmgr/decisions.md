# Decisions — EventMgr

---

## Receive stream: subscribe to broadcast before draining mailbox

**Decision:** In `Receive`, we subscribe to the live broadcast channel *first*, then drain the mailbox backlog. Events emitted between drain and subscribe could appear in both the backlog drain AND the live broadcast.

**Why:** The alternative (drain first, then subscribe) has a hard gap — events emitted in that window are lost entirely from the stream. With the current approach, worst case is a duplicate, which `seq_num` handles. Lost events are unrecoverable; duplicates are not.

**Trade-off:** Clients need to deduplicate on `seq_num` if they care. The proto comment already calls this out.

---

## emit() is O(n) over all subscriptions

**Decision:** On every emit, we `list_subscriptions()` and scan all of them to find durable matches.

**Why:** Simple, correct, zero extra state. At current scale (dozens of subscriptions, not millions) this is fine.

**If this becomes a problem:** Index subscriptions by source at subscribe time. `DashMap<source, Vec<registration_id>>` would make emit a keyed lookup instead of a full scan. Not needed yet.

---

## Sequence counters: DashMap<String, AtomicU64>

**Decision:** One `AtomicU64` per `"source::event_type"` key stored in a `DashMap`.

**Why:** Lock-free increment per key. No contention between different event types. The `DashMap::entry().or_insert_with()` gives us lazy init without a global lock.

**Constraint:** Never hold the `DashMap` ref across an await point. Seq counter increment is synchronous — this is fine as written.

---

## EventMgr is independent of Space

**Decision:** EventMgr uses `InMemoryEventStore` directly via the provider layer. It does not depend on Space (which doesn't exist yet).

**Why:** Space is Layer 5+. EventMgr is Layer 2. Introducing a dependency would violate boot order and couple two services that have no business being coupled. Events are coordination primitives, not data.

---

## No subscription reaper hookup (yet)

**Decision:** The lease reaper fires `expiry_tx` when leases expire, but EventMgr doesn't listen for event subscription lease expiry. Expired subscriptions stay in the store until explicitly cancelled.

**Why:** Didn't want to scope-creep the session. The pattern is established (Registry does this already). It's a one-function hookup. Parked.

**Risk:** Low. In-memory store. Stale subs accumulate but don't cause incorrect behavior — they just get events enqueued that nobody will ever drain.

---

## Port 9005

**Decision:** EventMgr binds to 9005. Proxy is on 9003, leaving 9004 open for future use (TransactionMgr candidate).

**Layer ordering for reference:**
| Port | Service |
|------|---------|
| 9001 | LeaseMgr |
| 9002 | Registry |
| 9003 | Proxy |
| 9004 | (reserved — TransactionMgr?) |
| 9005 | EventMgr |
