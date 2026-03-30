# Phase 2: Spec Alignment — Session 1 Complete

**Date:** 2026-03-30

## What Got Done

All 6 steps of Phase 2 spec alignment completed, plus SDK updates across Go/Java/Node.

### Step 1: Lease (commits `3ad2888`)
- Lease duration negotiation — server may grant less than requested
- `FOREVER` (0) and `ANY` (u64::MAX) constants
- `LeaseConfig` with `MAX_LEASE_TTL` / `PREFERRED_LEASE_TTL` env vars
- `ttl_seconds` field on Lease proto response

### Step 2: Registry (commit `681f005`)
- Re-registration via `capability_id` on RegisterRequest (Jini ServiceID pattern)
- `ModifyAttrs` RPC — add/remove attrs without re-registering
- `RegisterResponse` with capability_id + Lease (was returning bare Lease)
- `MODIFIED` event type (was `RENEWED`)
- **Critical fix:** Lease expiry cascade — zombie registry entries now cleaned up

### Step 3: ServiceDiscovery (commits `ae44474`, `8295f3e`, `300316d`)
- **Go SDK:** RegisterResult, CapabilityID re-registration, MODIFIED events, Watch reconnect with backoff
- **Java SDK:** register/modifyAttrs/watch on RegistryClient, Watch-based ServiceDiscovery cache invalidation (stale-on-expire, refresh-on-register, refresh-on-modified)
- **Node SDK:** Same as Java — RegisterResult, modifyAttrs, watch, Watch-based cache invalidation, reconnect on error

### Step 4: Events (commit `fa49d85`)
- **Critical fix:** Subscription lease expiry cascade — `remove_by_lease` on EventStore, `unsubscribe_by_lease` on EventManager, wired in main.rs
- Reviewed and confirmed aligned: pull-based Receive (deliberate), handback, seq numbers, subscribe-drain-live race, lease negotiation

### Step 5: Transactions (commit `2870ca2`)
- **Critical fix:** Lease expiry auto-abort cascade — `abort_expired()` on TxnManager, wired in main.rs
- Reviewed and confirmed aligned: 2PC state machine, PrepareAndCommit optimization, NOTCHANGED voter exclusion, idempotent commit/abort

### Step 6: Space (commits `cf51749`, `6bd7e1d`)
- **RPC renames to Jini spec:** Out→Write, Watch→Notify, CancelTuple→Cancel, RenewTuple→Renew
- **New:** Contents RPC (JavaSpace05.contents — bulk read streaming)
- **New:** handback on Notify/SpaceEvent (Jini MarshalledObject pattern)
- **Reserved:** txn_id field on Write/Read/Take/Notify/Contents (server ignores, wire-compatible for v2)
- All SDK stubs regenerated, all 3 SDKs build clean

## Recurring Theme: Lease Expiry Cascades

Every service had the same zombie problem — leases expired but dependent resources persisted. All four now wired:

| Service | Prefix | Cascade Action |
|---------|--------|---------------|
| Registry | `registry:` | Unregister entry + broadcast EXPIRED |
| EventMgr | `event:` | Remove subscription + mailbox |
| TxnMgr | `txn:` | Auto-abort all participants |
| Space | `space:` / `space-watch:` | Remove tuple / watch (already had it) |

## Docker Verification

Full stack tested in Docker Desktop — all 6 services responding:
- LeaseMgr: Grant with TTL negotiation ✓
- Registry: Greeter registered with transport ✓
- Proxy: Full ServiceDiscovery flow (greeter client → proxy → greeter service) ✓
- TxnMgr: Begin returns transaction + lease ✓
- EventMgr: Subscribe returns registration + lease ✓
- Space: Write + Read with Jini-named RPCs ✓

## Test Counts
- Rust: 34 tests (lease 4, registry 4, event 6, space 13, txn 7)
- All Go/Java/Node SDKs build clean

## What's NOT Done

**Transaction isolation on Space operations** — the `txn_id` field is reserved in the proto but the server ignores it. No uncommitted write buffer, no transaction-scoped visibility, Space doesn't enlist as a 2PC participant.

This is the next effort.
