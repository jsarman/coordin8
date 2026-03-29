# PRD: EventMgr

**Status:** Implemented
**Port:** 9005
**Layer:** 2b (peer to Registry — neither depends on the other)

---

## What It Is

EventMgr is the durable event delivery service for Coordin8. Modeled after `net.jini.core.event` (Jini Distributed Events Specification) — but pull-based and gRPC-native.

The short version: any service emits events. Other services subscribe to them. Subscriptions are leased — stop renewing and you disappear. Events carry sequence numbers so clients can detect gaps and deduplicate.

> "Describe what you need, not where it is." — same principle, applied to events.

---

## Two Delivery Modes

| Mode | Behavior |
|------|----------|
| **Durable** (default) | Per-subscription mailbox. Events queue while subscriber is offline. Reconnect → drain backlog + resume live stream. |
| **BestEffort** | Broadcast only. No mailbox. Not receiving? Event is gone. |

---

## Wire Protocol (event.proto)

```
Subscribe(source, template, delivery, ttl_seconds, handback) → EventRegistration
Receive(registration_id) → stream Event
Emit(source, event_type, attrs, payload) → Empty
RenewSubscription(registration_id, ttl_seconds) → Lease
CancelSubscription(registration_id) → Empty
```

- `EventRegistration` carries a `Lease` and the `seq_num` at subscribe time (Jini EventRegistration equivalent)
- `Event` carries `handback` — echoed from `SubscribeRequest`, opaque to the bus
- `seq_num` is monotonically increasing per `(source, event_type)` — use it for idempotency and gap detection

---

## Template Matching

Same operators as Registry (`contains:`, `starts_with:`, exact, or empty/`*` = any). Template matches against event `attrs` + `event_type` field. Empty template = match all events from that source.

Reuses `coordin8_registry::matcher` — no duplication.

---

## Architecture Decisions

See `decisions.md`.

---

## What's Not Here (Yet)

- **Subscription reaper** — expired leases currently don't auto-clean subscriptions from EventMgr. The lease reaper fires but nothing listens for event subscription expiry. Add a listener on the `expiry_tx` broadcast, same pattern as Registry cleanup. Park it for next session.
- **Go SDK EventClient** — next up after this. Follow `LeaseClient` pattern.
- **Replay / seek** — no persistent event log. Durable delivery is mailbox-only. Once drained, it's gone.
- **Dead letter** — if enqueue fails (sub not found), it's silently dropped. Good enough for now.
