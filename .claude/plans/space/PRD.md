# PRD: Space — Distributed Reactive Tuple Store

**Status:** v1 Implemented (Rust core)
**Port:** 9006
**Layer:** 2c (peer with Registry + EventMgr)

---

## What It Is

The Space is the core coordination primitive from JavaSpaces — a shared, leased, template-matched tuple store. Producers `out()` tuples; consumers `take()` or `read()` by template; watchers get push streams on match. Every tuple is leased through LeaseMgr.

This is the service that unlocks the higher-order patterns (Lens, Reflex, Sentry) and the full coordination model from the design napkin.

> **Spec reference:** https://river.apache.org/release-doc/current/specs/html/js-spec.html

> "Describe what you need, not where it is." — applied to data.

---

## Wire Protocol (space.proto)

```
Out(attrs, payload, ttl_seconds, written_by, input_tuple_id) → OutResponse { Tuple }
Read(template, wait, timeout_ms) → ReadResponse { Tuple? }
Take(template, wait, timeout_ms) → TakeResponse { Tuple? }
Watch(template, on, ttl_seconds) → stream SpaceEvent
RenewTuple(tuple_id, ttl_seconds) → Lease
CancelTuple(tuple_id) → Empty
```

- `Tuple` carries `attrs` (template matching target), `payload` (opaque bytes), `Provenance`, and `Lease`
- `Provenance` tracks `written_by`, `written_at`, and `input_tuple_id` (lineage)
- `SpaceEvent` carries type (APPEARANCE / EXPIRATION) + the tuple + occurred_at
- `wait=false` on Read/Take = ifExists semantics — returns None immediately if no match
- `wait=true, timeout_ms=0` = wait indefinitely for a match

---

## Template Matching

Same operators as Registry and EventMgr — reuses `coordin8_registry::matcher`:
- Exact match: `"price"`
- `contains:humidity`
- `starts_with:tampa`
- Wildcard: `""` or `"*"` = Any
- Missing field in template = match anything

---

## Blocking Semantics

**Read (blocking):** Subscribe to tuple broadcast, wait for matching tuple to appear. Non-destructive — the tuple stays in the Space.

**Take (blocking):** Subscribe to tuple broadcast, re-query store atomically on each notification. Handles race with competing takers — if the store's `take_match` returns None (someone else claimed it), keep waiting. One winner.

**Timeout:** If `timeout_ms > 0`, the wait is bounded. Returns None on expiry.

---

## Lease Integration

- Every tuple gets a lease: `space:{tuple_id}` resource ID
- Every watch gets a lease: `space-watch:{watch_id}` resource ID
- Lease expiry listener in main.rs dispatches to `SpaceManager.on_tuple_expired()` / `on_watch_expired()`
- Expired tuples are removed from store and broadcast on `expiry_tx` (fires expiration watches)

---

## What's Here (v1)

| Item | Status | Notes |
|------|--------|-------|
| Proto: SpaceService definition | Done | `proto/coordin8/space.proto` |
| Rust: SpaceManager | Done | `coordin8-space` crate, boots Layer 2c |
| out(attrs, payload, ttl, written_by, lineage) | Done | Leased, broadcasts to tuple_tx |
| take(template, wait, timeout_ms) | Done | Atomic claim+remove, blocking with race handling |
| read(template, wait, timeout_ms) | Done | Non-destructive, blocking or non-blocking |
| watch(template, on, ttl) | Done | Server-streaming push, appearance + expiration |
| RenewTuple / CancelTuple | Done | Lease management on stored tuples |
| TTL / lease on tuples | Done | Reaper integration via expiry_tx listener |
| TTL / lease on watches | Done | Same reaper integration |
| Provenance metadata | Done | written_by, written_at, input_tuple_id |
| Template matching | Done | Reuses `coordin8_registry::matcher` |
| InMemorySpaceStore | Done | DashMap, race-safe take_match, lease_index |
| Integration tests (12) | Done | Full coverage — see test list below |

---

## What's Deferred (v2)

| Item | Notes |
|------|-------|
| Transaction support | `txn_id` on operations, visibility isolation, Space as 2PC participant |
| `snapshot()` optimization | Pre-serialized template for repeated matching |
| Go SDK SpaceClient | Blocked on v1 stabilization |
| Java SDK | |
| Node SDK | |
| CLI: spaces list/read/out/watch | Blocked on Space SDK |

---

## Test Coverage

All in-process tests (no gRPC needed), using InMemorySpaceStore + InMemoryLeaseStore:

1. `out_and_read` — write, read back by template
2. `out_and_take` — write, take, verify second take returns None
3. `read_nonblocking_empty` — readIfExists on empty → None
4. `take_nonblocking_empty` — takeIfExists on empty → None
5. `take_blocking_unblocks_on_out` — spawn blocking taker, out a match, taker wakes
6. `read_blocking_timeout` — blocking read with timeout, no match → None after timeout
7. `template_matching` — exact, contains:, starts_with:, wildcard, miss
8. `tuple_lease_expiry` — short TTL, reaper fires, tuple gone
9. `watch_appearance` — subscribe to watch, out a tuple, receive broadcast event
10. `take_race_one_winner` — two concurrent takers, exactly one wins
11. `cancel_tuple_removes` — out, cancel, read → None
12. `provenance_tracking` — lineage from input tuple to derived tuple

---

## Architecture Decisions

See `decisions.md`.
