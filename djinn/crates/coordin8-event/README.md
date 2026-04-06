# coordin8-event

EventMgr — durable event delivery with leased subscriptions. Modeled after the Jini Distributed Events Specification.

## Where It Fits

Layer 2b (port `9005`). Subscriptions are leased through [`coordin8-lease`](../coordin8-lease/README.md) — let the lease expire and the subscription disappears. Two delivery modes:

- **DURABLE** — events are buffered in a per-subscription mailbox when no `Receive` stream is open. When the consumer reconnects, the mailbox drains in order before live delivery resumes.
- **BEST_EFFORT** — broadcast only, no buffering. Late subscribers miss whatever fired before they connected.

Each event carries a sequence number scoped to the subscription so receivers can detect gaps.

## Layout

```
src/
  lib.rs       re-exports EventManager + EventServiceImpl
  manager.rs   EventManager — Subscribe, Emit, Receive, Cancel, mailbox bookkeeping
  service.rs   tonic server impl for EventService
tests/         integration tests for durable + best-effort delivery
```

## Build / Test

```bash
cargo build -p coordin8-event
cargo test  -p coordin8-event
```

Live demo against a running Djinn:

```bash
mise r demo-events    # market-watch — subscribe, emit offline, drain, emit live
```

## Notes

- The Djinn binary spawns a task that watches lease expiry for `event:` resource ids and calls `unsubscribe_by_lease`
- Sequence numbers are per-subscription, not global
- DURABLE mailboxes have no built-in size cap in `local`; in `dynamo` they are bounded by what DynamoDB will return per page
