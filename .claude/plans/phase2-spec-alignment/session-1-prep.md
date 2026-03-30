# Phase 2: Spec Alignment — Session 1 Prep

**Goal:** Align Coordin8's Djinn services with the Jini specification, bottom-up. Not blind conformance — deliberate alignment where the spec is right, deliberate divergence where we have good reason.

**Philosophy:** The Jini spec was designed by Waldo, Arnold, Scheifler, Wollrath et al. at Sun Labs. The core design patterns are 30 years old and still hold. Phase 1 got the system running. Phase 2 sharpens each service against the spec through scenario-driven examination. We play out real coordination scenarios, find where our semantics differ from the spec, and decide together: align or diverge.

---

## How We Work This

### Step 0: Systems Test What We Have

Before changing anything, exercise the full stack end-to-end. Use grpcurl against a running Djinn to walk through real scenarios. Document:

- **What works as expected** — confirm the happy paths
- **What's missing** — operations the spec defines that we don't have
- **What's wrong** — semantics that differ from spec in ways that matter
- **What's questionable** — things that work but might break under real coordination patterns

grpcurl cheat sheet:
```bash
# List services on a port
grpcurl -plaintext localhost:9001 list

# Describe a service
grpcurl -plaintext localhost:9006 describe coordin8.SpaceService

# Call an RPC
grpcurl -plaintext -d '{"resource_id":"test","ttl_seconds":30}' localhost:9001 coordin8.LeaseService/Grant
```

### Step 1: Lease (net.jini.core.lease)

The bedrock. Everything leases through LeaseMgr.

**Spec concepts to examine:**
- `Lease` interface: `getExpiration()`, `cancel()`, `renew(duration)`, `canBatch(Lease)`
- Lease duration negotiation — client requests duration, server may grant less
- `FOREVER` and `ANY` duration constants
- Lease maps / batch renewal (`LeaseRenewalManager` pattern)
- `LeaseListener` — notification when renewal fails
- Lease expiration as a first-class coordination event (not an error)

**Known gaps to probe:**
- Do we negotiate duration or just grant what's asked?
- Do we support batch renewal?
- Is `FOREVER` a concept we need?
- How does lease expiry propagate to dependent services? (Registry entries, Event subscriptions, Space tuples, Watches)

### Step 2: Registry (net.jini.core.lookup / ServiceRegistrar)

Service discovery with leased registrations.

**Spec concepts to examine:**
- `ServiceRegistrar` interface: `register(ServiceItem, leaseDuration)`, `lookup(ServiceTemplate)`, `lookup(ServiceTemplate, maxMatches)`
- `ServiceItem` = serviceID + service proxy + attribute sets
- `ServiceTemplate` = serviceID + serviceTypes[] + attributeSetTemplates[]
- Type-based matching (Java class hierarchy) — how does this map to our attribute model?
- `ServiceEvent` types: TRANSITION_MATCH_NOMATCH, TRANSITION_NOMATCH_MATCH, TRANSITION_MATCH_MATCH
- `ServiceID` — UUID assigned by registrar on first registration, not by the client
- Attribute modification: `modifyAttributes()`, `addAttributes()`
- Entry classes (typed attribute sets) vs our flat `map<string,string>`

**Known gaps to probe:**
- We don't have ServiceID assignment — clients pick their own identity
- Our "register" creates a new entry every time — spec has modify/update semantics
- Watch events are simpler than the spec's three transition types
- No attribute modification after registration
- Typed entries vs flat string maps — deliberate divergence or gap?

### Step 3: ServiceDiscovery (net.jini.discovery / LookupDiscoveryService)

Client-side discovery + caching layer.

**Spec concepts to examine:**
- `LookupDiscovery` — multicast/unicast discovery of lookup services
- `ServiceDiscoveryManager` — client cache with event notification
- `LookupCache` — local cache that tracks service changes
- `ServiceDiscoveryListener` — callbacks for serviceAdded, serviceRemoved, serviceChanged
- Discovery groups and locators

**Known gaps to probe:**
- Java/Node caches never refresh (known gap)
- No discovery protocol — clients connect to known addresses
- No concept of discovery groups
- Cache invalidation on lease expiry — does it work end-to-end?

### Step 4: Events (net.jini.core.event / RemoteEventListener)

Distributed event delivery.

**Spec concepts to examine:**
- `RemoteEvent`: eventID, seqNum, source, handback
- `RemoteEventListener.notify(RemoteEvent)` — the push model
- `EventRegistration`: source, eventID, lease, seqNum at registration time
- Event ID vs sequence number — eventID identifies the *kind* of event, seqNum orders instances
- Unknown event contract — what happens when listener gets an event it doesn't understand?
- Third-party event delegation (event mailbox service)

**Known gaps to probe:**
- Our `Receive` is pull-based (server-streaming) vs spec's push (listener callback) — deliberate
- `source` semantics — our source is a string name, spec's is the remote object that registered
- Subscription reaper hookup — expired subs don't auto-clean (known gap from EventMgr PRD)
- Do we correctly handle the "subscribe, then drain backlog, then live" race?

### Step 5: Transactions (net.jini.core.transaction)

2PC coordination.

**Spec concepts to examine:**
- `TransactionManager`: `create(lease)` → `Created(id, lease)`
- `Transaction.Created` — carries both the transaction and the lease
- `TransactionParticipant`: `prepare()`, `commit()`, `abort()`, `prepareAndCommit()`
- Vote semantics: PREPARED, NOTCHANGED, ABORTED
- `ServerTransaction` vs `NestableServerTransaction` (nested txn — almost certainly defer)
- Manager crash recovery — transaction log durability
- Participant crash count — detect participant restarts between prepare and commit

**Known gaps to probe:**
- Crash recovery — we have none (in-memory only)
- Nested transactions — not implemented, probably defer
- Lease on transaction — does expiry correctly abort?
- `crashCount` — we accept it but do we use it?

### Step 6: Space (net.jini.space.JavaSpace)

Revisit with fresh eyes after aligning the layers beneath it.

**Spec concepts to examine:**
- `JavaSpace05` extensions: `contents()`, `registerForAvailabilityEvent()`
- Entry serialization — typed fields vs our attrs map
- `snapshot()` — pre-serialized entry for repeated use
- Transaction isolation — uncommitted writes invisible to other readers
- `notify()` vs our `watch()` — spec notification includes transition type
- Ordering guarantees — what does a watcher see relative to the operation?
- `read`/`readIfExists` null semantics
- `take` fairness — spec doesn't guarantee FIFO but has ordering expectations

**Deferred from this pass:**
- Transaction support on Space operations (v2)
- `snapshot()` optimization

---

## Work Pattern

For each service layer:

1. **Read the relevant Jini spec section** — understand what they intended
2. **Exercise our implementation** — grpcurl + existing tests, play out scenarios
3. **List divergences** — categorize as: align (fix it), diverge (document why), defer (later)
4. **Discuss** — align on each decision together before coding
5. **Implement changes** — bubble fixes up to dependent services as needed
6. **Keep the build green** — every change must pass `cargo test --all` + `cargo clippy --all`

---

## Scenario Bank (to play out during testing)

These are coordination scenarios that stress the spec contracts:

1. **Lease cascade** — Service registers, lease expires, registry entry disappears, proxy connection fails over, client discovers new service. Does the full cascade work?
2. **Take race** — Three consumers blocking take on same template. One tuple arrives. Exactly one winner. Losers keep waiting.
3. **Watch ordering** — Watch for appearances, then out + take rapidly. Does the watcher see the appearance? Does it see the expiration (from take's lease cancel)?
4. **Event gap detection** — Subscriber connects, drains backlog, goes offline, reconnects. Are sequence numbers correct for gap detection?
5. **Transaction abort cascade** — Begin txn, enlist two participants, one vetoes. Does abort reach both? Does the lease get cancelled?
6. **Lease renewal failure** — Service renewing its registry entry lease. Djinn goes away mid-renewal. Client discovers the service is gone.
7. **Space blocking read + expiry** — Blocking read on a template. Tuple that would match expires before the reader gets it. Reader should NOT see it.
8. **ServiceDiscovery cache coherence** — Service registered, client caches proxy. Service re-registers on different port. Does the cache update?

---

## What to Bring to Session 1

- A running Djinn (local, `cargo run` or `mise r djinn`)
- grpcurl installed
- The Jini spec open: https://river.apache.org/release-doc/current/specs/html/
- Specific sections:
  - Lease: `js-spec.html` §LJ (Jini Distributed Leasing Specification)
  - Lookup: `js-spec.html` §DJ (Jini Lookup Service Specification)
  - Events: `js-spec.html` §EV (Jini Distributed Events Specification)
  - Transactions: `js-spec.html` §TX (Jini Transaction Specification)
  - JavaSpaces: `js-spec.html` §JS (JavaSpaces Specification)
