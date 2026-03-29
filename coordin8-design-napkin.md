# Coordin8 — Design Napkin

## The Thesis

The cloud world has durable, scalable building blocks (DynamoDB, SQS, EventBridge, Kafka, Kinesis) but no coordination theory on top. Developers stitch together REST calls and pretend it's distributed computing. The result is distributed monoliths — all the coupling, none of the transactional guarantees.

Coordin8 brings back the coordination model that Sun Labs got right in 1999 with Jini/JavaSpaces (Linda tuple spaces), but decoupled from the JVM, RMI, and multicast discovery. Same primitives, modern runtime, any cloud.

**One sentence pitch:** "Describe what you need, not where it is."

---

## Origins / Credibility

- Jini heritage: built a distributed weather station sensor network across Florida using JavaSpaces for coordination and smart proxies for data streaming
- Built a Jini-based stock symbol scanner that detected pump activity across channels via coordinated mention velocity — the coordination model worked, the architecture was sound 20 years before the crypto ecosystem made it trivially executable
- Long-standing background in distributed coordination, IoT sensor networks, and platform architecture (currently Principal Architect designing shared infrastructure on AWS/EKS)

---

## Core Mental Model

Coordin8 is a **write-and-notify tuple space** with durable semantics, lease-based liveness, distributed transactions, and pluggable transport/storage providers.

Think of it as JavaSpaces where:
- The JVM is replaced by any cloud provider (or local SQLite)
- RMI is replaced by pluggable transports (Kafka, Kinesis, gRPC, TCP, etc.)
- Multicast discovery is replaced by a registry with attribute-based template matching
- The tuple space, lease manager, transaction manager, event manager, and registry are five independent composable services — **The Djinn**

---

## The Djinn — Five Core Services

All independent. All use each other. All leased. Named after the Jini heritage (Jini → djinn/genie).

### Layered Boot Sequence

```
Layer 0:  Provider (DynamoDB, SQLite, CosmosDB, etc.)
          Raw storage. No coordination semantics.

Layer 1:  LeaseMgr
          Built directly on provider. No dependencies.
          Bedrock — everything else trusts this.

Layer 2:  Space ◄──► EventMgr  (peers, neither depends on the other)
          Space: tuple operations with lease support
          EventMgr: durable event delivery, store-and-forward

Layer 3:  TransactionMgr
          Uses LeaseMgr for transaction leases.
          Uses provider directly for journal.
          Any service enlists as a participant.

Layer 4:  Registry
          Implemented AS a space with leased entries.
          Pure pattern on top of existing primitives.
```

No circular dependencies. LeaseMgr is the bedrock. Everything builds upward cleanly.

### Bootstrap

```python
djinn = Djinn(provider="aws", region="us-east-1")
djinn.start()

space = djinn.space("orders")
tx = djinn.transaction_manager()
registry = djinn.registry()
events = djinn.event_manager()
```

---

## Service 1: LeaseMgr

The bedrock. Failure is not exceptional, it's expected. Leases make everything self-healing.

A lease is a **liveness contract** — "this fact is only true for a bounded period unless someone actively renews it." The absence of renewal IS the signal.

```python
lease = await lease_mgr.grant(resource_id="worker-7", ttl=30)
await lease.renew(30)   # I'm still here
await lease.cancel()    # I'm done

# Or let it die — that IS the event
```

### What leases enable

- **Leader election** — write a "leader" tuple with TTL, renew via heartbeat. Leader dies → tuple expires → nodes race to claim leadership via `take()`
- **Distributed locks** — `out()` a lock tuple with TTL. Holder crashes → lock auto-releases. No deadlocks ever.
- **Circuit breakers** — "circuit open" tuple with 30s TTL. Auto-closes on expiry unless renewed.
- **Session management** — user session is a leased tuple. No heartbeat → session expires. No cleanup jobs.
- **Failure detection** — "expected silence" pattern: write a tuple with TTL, if nobody acts on it before expiry, the expiration watch fires an alert for the thing that *didn't happen*.

---

## Service 2: Space

A named, distributed, reactive coordination store with tuple semantics.

### Primitives (maps to JavaSpace interface)

| JavaSpace | Coordin8 | Description |
|---|---|---|
| `write(entry, txn, lease)` | `out(tuple, txn, ttl)` | Write a fact into the space |
| `take(tmpl, txn, timeout)` | `take(template, txn, wait=True)` | Atomically claim and remove. One consumer wins. |
| `takeIfExists(tmpl, txn, timeout)` | `take(template, txn, wait=False)` | Non-blocking take |
| `read(tmpl, txn, timeout)` | `read(template, txn, wait=True)` | Non-destructive read. Fact stays. |
| `readIfExists(tmpl, txn, timeout)` | `read(template, txn, wait=False)` | Non-blocking read |
| `notify(tmpl, txn, listener, lease, handback)` | `watch(template, handler, on, ttl)` | Reactive push on match |
| `snapshot(entry)` | *(provider-level optimization)* | Pre-serialized template for hot paths |

### Key design points

- Every operation optionally takes a `txn` — coordination is never an afterthought
- Every operation has time semantics (TTL, timeout, lease) — you MUST think about temporal bounds
- Template matching: null/missing fields mean "match anything"
- `watch()` supports both appearance and **expiration** events (`on="expire"`)
- Tuples carry automatic **provenance** metadata — who wrote it, when, what input produced it

### TTL / Expiration watches

TTL isn't cleanup — it's a coordination primitive (the lease model applied to data):

```python
lease = await space.out(
    {"node": "worker-7", "status": "alive"},
    ttl=30
)
await lease.renew(30)

@watch("workers", on="expire")
async def worker_died(expired_tuple):
    await space.out({"alert": f"{expired_tuple['node']} went silent"})
```

---

## Service 3: EventMgr

Independent from Space. Any service can emit and receive events, not just spaces.

### Delivery modes

```python
# Durable — events queue if listener is offline (mailbox semantics)
sub = await event_mgr.subscribe(
    source="market.signals",
    template={"token": "SOL"},
    delivery="durable"
)

# Best-effort — fire and forget, miss it if you're not there
sub = await event_mgr.subscribe(
    source="market.signals",
    template={"token": "SOL"},
    delivery="best-effort"
)

# Subscriptions are leased
sub = await event_mgr.subscribe(
    source="market.signals",
    template={"token": "SOL"},
    delivery="durable",
    ttl=3600
)
```

### Any service can emit events

```python
class DexExecutor(TransactionParticipant, EventSource):
    async def execute(self, order):
        result = await self._submit(order)
        await self.emit({"type": "fill", "order_id": order["id"], "price": result["price"]})
```

This prevents the "lossy EventBridge" problem — durable delivery means events survive listener downtime.

---

## Service 4: TransactionMgr

**Real distributed transactions, not sagas.** Step Functions is a saga orchestrator with compensation logic. Coordin8 provides actual two-phase commit.

### The Jini model

In Jini, the `TransactionManager` was independent. Anything could be a transaction participant by implementing `TransactionParticipant`. The space participated, but so could any custom service.

### Participant interface

```python
class TransactionParticipant:
    async def prepare(self, txn_id) -> Vote:   # COMMIT or ABORT
    async def commit(self, txn_id) -> None
    async def abort(self, txn_id) -> None
```

### Usage — transactions span any participants

```python
async with tx_mgr.begin(ttl=30) as txn:
    # Space operation — built-in participant
    await signals.take(template, txn=txn)
    
    # Custom participant
    await dex_executor.submit(order, txn=txn)
    
    # Another custom participant
    await postgres_ledger.insert(record, txn=txn)
    
    # ALL commit or ALL rollback
    # If process dies, txn lease expires, all participants abort
```

### Implementation strategy

**Same-provider optimization:** If all spaces share a DynamoDB backend → `TransactWriteItems` (up to 100 atomic operations). Real atomicity, no saga.

**Cross-provider:** Lightweight purpose-built 2PC coordinator:

```
Phase 1 (Prepare):
  - Each participant journals intent (write-ahead log)
  - Each participant locks affected tuples
  - Each participant votes commit/abort

Phase 2 (Commit/Abort):
  - All voted commit → apply, release locks
  - Any voted abort → discard, release locks

Recovery:
  - Coordinator crashes → journal survives (DynamoDB)
  - Restart reads journal, completes or aborts in-flight txns
  - Participant crashes → lock is a leased tuple, expires on its own
```

**The transaction manager is built from Coordin8 primitives:**
- Journal entries are tuples in a space
- Locks are leased takes
- Timeout/failure detection is lease expiry
- Recovery is watching for expired journal entries

---

## Service 5: Registry

Attribute-based service discovery. No hardcoded addresses. Ever.

**In Jini this was Reggie.** Services registered by interface and attributes. Consumers found services by template matching on capabilities, not by DNS or endpoint URLs.

### Registration (leased)

```python
lease = await registry.register(
    interface="WeatherStation",
    attrs={
        "region": "tampa-east",
        "station": 7,
        "metrics": ["wind", "humidity", "rain", "temp"]
    }
)
# Stop renewing → disappear from registry. No stale entries.
```

### Lookup by template (not by address)

```python
# "Give me a weather station in Tampa with humidity"
station = await registry.lookup({
    "interface": "WeatherStation",
    "region": "tampa-east",
    "metrics": {"contains": "humidity"}
})

stream = await station.connect(metrics=["humidity"])
# No IP. No port. No protocol. No endpoint URL.
# The Djinn resolved everything.
```

### Watch for service changes

```python
@registry.watch({"interface": "TradeExecutor"})
async def on_executor_change(event):
    if event.type == "expired":    # executor died
        pass  # failover logic
    if event.type == "registered": # new executor available
        pass  # rebalance
```

### How it differs from Kubernetes service discovery

K8s gives you DNS-based discovery — `service.namespace.svc.cluster.local`. That tells you *where* something is. The Registry tells you *what* it is. You can't ask k8s "find me any TradeExecutor that handles Solana." You can't template match on attributes. You can't watch for capability changes. And k8s health checks are binary (alive/dead). Lease-based registration is continuous liveness assertion.

### Implementation

The Registry IS a Space with leased entries. `register()` is `out()` with a lease. `lookup()` is `read()` with template matching. `watch()` is `watch()`. No new infrastructure — pure pattern.

---

## Core Concepts

### Tuple
A JSON object representing a fact. Any shape. Optional schema registration for production hardening.

### Template
A partial tuple used for matching. Missing/null fields match anything. Supports equality, `contains`, `starts_with`, `gte/lte`, range queries.

### Lease
A bounded liveness contract. Renew to persist, let expire to signal absence. Baked into every operation, not bolted on.

### Capability (Smart Proxy)
An object returned from the Registry that knows how to connect you to the real thing. The consumer never sees IPs, ports, or protocols. The Capability resolves everything.

### Transaction / Participant
Two-phase commit across any services that implement the Participant interface. Not limited to spaces.

---

## The Capability / Information Pattern

### Smart proxies — not addresses

In Jini, what you got back from Reggie wasn't data about a service — it was a live object that could do things. In Coordin8, the Capability handle returned from `lookup()` is wired up by the Djinn with everything needed to connect.

### The Information pattern

A smart proxy that uses space-based indexes to provide random access into data streams. The space is the index. The stream is the data plane. The Capability is the query engine.

```
Consumer:  "give me 9am to 10am of humidity data"
    │
Information reads index tuples from space:
    "9:00-9:05 → offset 44032"
    "9:05-9:10 → offset 44128"
    │
Information tells transport: seek to 44032, stream through 58819
    │
Consumer gets: stream of humidity readings
```

**Key principle:** The space never carries the heavy data. It carries the *knowledge about* the data — where it is, what time range it covers, how to reach it.

### Transport agnosticism via capabilities

The transport is an implementation detail behind the Capability. Kafka, Kinesis, TCP, gRPC — invisible to the consumer unless they choose to filter on it.

```python
# Consumer doesn't care about transport
station = await registry.lookup({
    "interface": "WeatherStation",
    "metrics": {"contains": "humidity"}
})

# Consumer CAN care if they want
station = await registry.lookup({
    "interface": "WeatherStation",
    "metrics": {"contains": "humidity"},
    "transport": "kafka"
})
```

### Backtesting via Information

Same interface, different source. Live trading and backtesting use identical code.

```python
# Live
info = await registry.lookup({"interface": "MarketData", "token": "SOL"})

# Backtest — same code, same Lenses, same Sentry
info = await registry.lookup({"interface": "MarketData", "token": "SOL", "mode": "historical"})

# Paper trading — live data, fake execution
info = await registry.lookup({"interface": "MarketData", "token": "SOL", "mode": "paper"})
```

Three modes, one codebase. Only the Registry entries change.

---

## Providers — Three Dimensions

```
Storage:    SQLite | DynamoDB | CosmosDB | Firestore
Transport:  Kafka | Kinesis | NATS | TCP | gRPC | Redis Streams
Compute:    Lambda | EKS | Local process
```

Mix and match. The Djinn wires it up.

### Developer Experience Tiers

```
Dev:         SQLite in-process. pip install and go. Zero cloud dependencies.
Integration: LocalStack on Docker Desktop. Real AWS semantics, local.
Production:  Real AWS (or Azure, GCP). Same code, different endpoint.
```

```python
djinn = Djinn(provider="local")                                    # dev
djinn = Djinn(provider="aws", endpoint="http://localhost:4566")    # localstack
djinn = Djinn(provider="aws", region="us-east-1")                 # prod
```

One line difference. Mental model doesn't change.

### docker-compose for dev

```yaml
services:
  localstack:
    image: localstack/localstack
    ports:
      - "4566:4566"
    environment:
      - SERVICES=dynamodb,sqs,events
  
  dashboard:
    image: coordin8/dashboard
    ports:
      - "8080:8080"
```

---

## Higher-Order Patterns

Not in core. Built on primitives. Patterns library / examples.

| Pattern | Description | Built from |
|---|---|---|
| **Lens** | Reactive derived computation. Watches a space, computes, writes to another space. | `watch()` + function + `out()` |
| **Reflex** | Action executor. Takes intent tuples, executes against external systems, writes results. | `take()` + side effect + `out()` |
| **Sentry** | Constraint watcher. Monitors spaces for violations, writes alerts or kill-switch tuples. | `watch()` + predicate + `out()` |
| **Ledger** | Append-only audit space. Every `out()` is immutable. No `take()`. Full trail. | Space with delete disabled |
| **SignalMesh** | External data ingest. Connectors that normalize external feeds into tuples. | Connectors + `out()` |
| **GutLog** | Human-in-the-loop. Log intuition as tuples. Correlate against outcomes later. | `out()` + Lens for correlation |
| **Information** | Smart proxy with space-indexed stream access. Random access queries over continuous data. | Registry + Space indexes + Transport |

### Lens example

```python
@lens(watch="market.signals", output="trade.signals")
async def momentum_scorer(signal):
    if signal["type"] == "liquidity_spike":
        score = compute_momentum(signal)
        if score > 0.7:
            return {"token": signal["token"], "score": score, "basis": "momentum"}
```

No REST endpoints. No SQS polling boilerplate. No retry logic. No k8s manifests. Write functions that react to tuples and produce tuples.

---

## Cross-Cutting Concerns

### Schema Registry
Optional schema registration per space. Validate on `out()`, warn or reject on mismatch. Developers discover schemas by reading them from the space itself.

### Backpressure
Configurable per `watch()` — buffer and batch, drop oldest, drop newest, sample. Without this: unbounded memory growth or silent data loss.

### Provenance / Trace
Every tuple gets automatic metadata — who wrote it, when, which Lens produced it, what input tuples it derived from. Enables full causal chain tracing.

### Space Topology / Bridge
`bridge()` connects two spaces — tuples matching a template in space A automatically appear in space B. Cross-domain flows without tight coupling.

---

## CLI

```bash
coordin8 spaces list
coordin8 read market.signals --last 10
coordin8 out gut.calls '{"token":"SOL","feeling":"heavy"}'
coordin8 watch trade.intents
coordin8 trace <tuple-id>       # full causal chain — killer feature
```

---

## Repo Structure (Open Source)

```
coordin8/
  core/
    djinn.py
    space.py
    lease_mgr.py
    transaction_mgr.py
    event_mgr.py
    registry.py
    participant.py
    capability.py
  providers/
    local/              # SQLite
    aws/                # DynamoDB + EventBridge + SQS
  patterns/
    lens.py
    reflex.py
    sentry.py
    ledger.py
    information.py
  cli/
  dashboard/
  terraform/
    aws/
  docker-compose.yml
  examples/
    iot-sensor-mesh/    # public example — weather stations
    task-queue/         # public example — distributed task queue
```

---

## What Coordin8 Is NOT

- Not a message broker (it uses them underneath)
- Not a database (it uses them underneath)
- Not a service mesh (it replaces the need for one)
- Not a saga orchestrator (it provides real transactions)
- Not a framework (it's primitives that compose)

## What Coordin8 IS

Five independent services — Space, LeaseMgr, EventMgr, TransactionMgr, Registry — that compose into a full distributed coordination platform. Three insights from Jini, modernized:

1. **Coordination is a small set of primitives**, not a framework
2. **Failure is expected**, not exceptional — leases make everything self-healing
3. **Services compose through shared coordination**, not by calling each other

The cloud providers built the lamp. Coordin8 is the genie.

---

## Technology Stack

### Language Decisions

| Component | Language | Rationale |
|---|---|---|
| Core services (Djinn) | **Rust** | Fast, concurrent, small footprint. Runs as sidecar or standalone daemon. Memory safety without GC pauses. |
| CLI (`coordin8`) | **Go** | Fast compilation, single binary distribution, natural fit for CLI tooling. |
| Client SDKs | **Polyglot** | Thin wrappers over gRPC. Go first, then Python, Java/Kotlin, TypeScript, Rust. |
| Dashboard | **TypeScript/React** | Browser-based observability UI. |
| Terraform modules | **HCL** | Infrastructure provisioning per cloud provider. |

### Wire Protocol — gRPC + Protobuf

gRPC replaces RMI. Protobuf replaces Java Serialization. Same roles, modern transport, no language lock-in.

| Jini | Coordin8 |
|---|---|
| Java Serialization | Protobuf |
| RMI | gRPC |
| JERI (Jini Extensible Remote Invocation) | gRPC interceptors/middleware |
| Smart Proxy (serialized Java object) | Capability descriptor + SDK hydration |
| Multicast discovery | DNS / well-known endpoint / Registry bootstrap |

**Why gRPC:**
- Protobuf schema definitions = code-gen SDKs for any language
- Streaming support for `watch()` and `Information` pattern
- Bidirectional streams for transport proxying
- Proto definitions ARE the contract

**Smart Proxy without shipping code:** Jini's smart proxy was executable code deserialized on the client — powerful but a security nightmare. Coordin8 returns a Capability *descriptor* over gRPC. The SDK reads the descriptor and hydrates it into a language-native smart proxy using a local registry of transport handlers. Same polymorphism, no executable code crossing the wire.

```protobuf
message Capability {
    string interface = 1;
    map<string, string> attrs = 2;
    TransportDescriptor transport = 3;
    repeated StreamDescriptor streams = 4;
}

message TransportDescriptor {
    string type = 1;                    // "kafka", "grpc", "kinesis"
    map<string, string> config = 2;     // transport-specific, resolved by SDK
}
```

### Protobuf Service Definitions (Core)

```protobuf
service LeaseService {
    rpc Grant(GrantRequest) returns (Lease);
    rpc Renew(RenewRequest) returns (Lease);
    rpc Cancel(CancelRequest) returns (Empty);
    rpc WatchExpiry(WatchExpiryRequest) returns (stream ExpiryEvent);
}

service RegistryService {
    rpc Register(RegisterRequest) returns (Lease);
    rpc Lookup(LookupRequest) returns (Capability);
    rpc LookupAll(LookupRequest) returns (stream Capability);
    rpc Watch(RegistryWatchRequest) returns (stream RegistryEvent);
}

service SpaceService {
    rpc Out(OutRequest) returns (Lease);
    rpc Take(TakeRequest) returns (stream TupleResponse);
    rpc Read(ReadRequest) returns (stream TupleResponse);
    rpc Watch(SpaceWatchRequest) returns (stream WatchEvent);
}

service TransactionService {
    rpc Begin(BeginRequest) returns (Transaction);
    rpc Prepare(PrepareRequest) returns (Vote);
    rpc Commit(CommitRequest) returns (Empty);
    rpc Abort(AbortRequest) returns (Empty);
}

service EventService {
    rpc Subscribe(SubscribeRequest) returns (Lease);
    rpc Emit(EmitRequest) returns (Empty);
    rpc Receive(ReceiveRequest) returns (stream Event);
}
```

### Polyglot SDK Architecture

```
┌──────────┐ ┌──────────┐ ┌──────────┐ ┌──────────┐
│ Go SDK   │ │ Python   │ │ Java/    │ │ TS SDK   │
│ (first)  │ │ SDK      │ │ Kotlin   │ │          │
│          │ │          │ │ SDK      │ │          │
└────┬─────┘ └────┬─────┘ └────┬─────┘ └────┬─────┘
     │            │            │             │
     └────────────┴─────┬──────┴─────────────┘
                        │
                 Wire Protocol (gRPC)
                        │
          ┌─────────────┴──────────────┐
          │        The Djinn (Rust)    │
          │                            │
          │  LeaseMgr                  │
          │  Space                     │
          │  EventMgr                  │
          │  TransactionMgr            │
          │  Registry                  │
          │                            │
          │  Provider (AWS/Local)      │
          └────────────────────────────┘
```

**SDK responsibilities (thin):**
- Wrap gRPC calls in idiomatic language patterns
- Manage lease renewal heartbeats in background goroutines/threads
- Handle reconnection and retry
- Hydrate Capability descriptors into language-native smart proxies
- Provide pattern helpers (@lens decorators in Python, functional builders in Go, etc.)

**TransactionParticipant works polyglot:** A Go service and a Python service participate in the same transaction because 2PC coordination happens over gRPC. The Djinn sends prepare/commit/abort over the wire — doesn't care what language handles them.

---

## Phased Build Plan

### Phase 0 — Foundation

**Goal:** Rust project scaffolding, protobuf definitions, build pipeline.

**Deliverables:**
- Rust workspace with crate structure: `coordin8-core`, `coordin8-lease`, `coordin8-registry`, `coordin8-proto`
- Protobuf definitions for LeaseService and RegistryService
- gRPC server skeleton using `tonic`
- Local provider (SQLite via `rusqlite` or in-memory)
- CI pipeline (GitHub Actions: build, test, lint)

**Repo structure:**

```
coordin8/
  proto/
    coordin8/
      lease.proto
      registry.proto
      common.proto
  djinn/                        # Rust workspace
    Cargo.toml
    crates/
      coordin8-proto/           # generated protobuf code
      coordin8-core/            # shared types, provider trait
      coordin8-lease/           # LeaseMgr implementation
      coordin8-registry/       # Registry implementation
      coordin8-djinn/          # binary, bootstraps services
    providers/
      local/                    # SQLite / in-memory
  cli/                          # Go module
    go.mod
    cmd/
      coordin8/
        main.go
  sdks/
    go/                         # Go client SDK
      go.mod
      coordin8/
        client.go
        lease.go
        registry.go
  examples/
    hello-coordin8/
      greeter_service/          # Go
      greeter_client/           # Go
  docker-compose.yml
  README.md
```

### Phase 1 — LeaseMgr (The Bedrock)

**Goal:** Working lease manager in Rust, accessible over gRPC.

**Deliverables:**
- `LeaseService` gRPC server in Rust
- Grant, Renew, Cancel operations
- TTL-based expiration with background reaper
- Expiry watch stream (server-side streaming gRPC)
- Local provider: in-memory with optional SQLite persistence
- Go client SDK: `coordin8.LeaseClient`
- Go CLI: `coordin8 lease grant`, `coordin8 lease renew`, `coordin8 lease list`

**Lease internals (Rust):**

```rust
pub trait LeaseStore {
    fn grant(&self, resource_id: &str, ttl: Duration) -> Result<LeaseRecord>;
    fn renew(&self, lease_id: &str, ttl: Duration) -> Result<LeaseRecord>;
    fn cancel(&self, lease_id: &str) -> Result<()>;
    fn expired(&self) -> Result<Vec<LeaseRecord>>;
}
```

**Test cases:**
- Grant a lease, verify it exists
- Let lease expire, verify expiry event fires
- Renew before expiry, verify extension
- Cancel explicitly, verify removal
- Concurrent renewals from multiple clients

### Phase 2 — Registry (Reggie Reborn)

**Goal:** Attribute-based service discovery with leased registrations. Needs a name — Reggie is the spiritual ancestor but Coordin8's registry deserves its own identity.

**Name candidates for the Registry:**
- **Beacon** — services light up, consumers find them
- **Lantern** — same vibe, fits the Djinn/lamp theme
- **Signal** — too overloaded
- **Summon** — you summon a capability from the registry
- **Manifest** — services manifest themselves, consumers discover them

**Deliverables:**
- `RegistryService` gRPC server in Rust
- Register with interface + attributes + TTL (delegates to LeaseMgr)
- Lookup by template matching (partial match, `contains`, `starts_with`, range)
- LookupAll returns stream of all matches
- Watch for registration/expiration events (server-side streaming)
- Capability descriptor returned on lookup
- Go client SDK: `coordin8.RegistryClient`
- Go CLI: `coordin8 registry list`, `coordin8 registry lookup`, `coordin8 registry watch`

**Template matching engine (Rust):**

```rust
pub enum MatchOp {
    Eq(Value),
    Contains(Value),
    StartsWith(String),
    Gte(Value),
    Lte(Value),
    Range(Value, Value),
    Any,  // null/missing = match anything
}

pub fn matches(template: &Template, entry: &Entry) -> bool {
    template.fields.iter().all(|(key, op)| {
        match entry.attrs.get(key) {
            Some(val) => op.matches(val),
            None => matches!(op, MatchOp::Any),
        }
    })
}
```

**Test cases:**
- Register service, lookup by interface — found
- Register service, lookup by interface + attribute — found
- Register service, lookup by wrong attribute — not found
- Register two services, lookupAll — both returned
- Kill service, lease expires, lookup — not found
- Watch: see registration event, see expiration event
- Template matching: contains, starts_with, range queries

### Phase 3 — Hello Coordin8

**Goal:** End-to-end demo. Two separate processes discover each other through the Registry with zero hardcoded addresses.

**Demo flow:**

```
Terminal 1: Start the Djinn
─────────────────────────
$ coordin8 djinn start --provider local
Djinn starting...
  ✓ Provider: local (in-memory)
  ✓ LeaseMgr: listening on :9001
  ✓ Registry: listening on :9002
Djinn ready.

Terminal 2: Start greeter service
─────────────────────────────────
$ cd examples/hello-coordin8/greeter_service
$ go run main.go
Registering with Djinn...
  ✓ Registered as Greeter (language=english)
  ✓ Lease: 30s (auto-renewing)
Serving...

Terminal 3: Run greeter client
──────────────────────────────
$ cd examples/hello-coordin8/greeter_client
$ go run main.go --name "Coordin8"
Looking up Greeter...
  ✓ Found: Greeter on <node>
  → Hello, Coordin8!

Terminal 3: Kill the service (Ctrl+C in Terminal 2), wait 30s
─────────────────────────────────────────────────────────────
$ go run main.go --name "Coordin8"
Looking up Greeter...
  ✗ No Greeter available (lease expired)
```

**greeter_service/main.go:**

```go
package main

import (
    "fmt"
    "github.com/coordin8/sdk-go/coordin8"
)

type Greeter struct{}

func (g *Greeter) Hello(name string) string {
    return fmt.Sprintf("Hello, %s!", name)
}

func main() {
    djinn := coordin8.Connect("localhost:9002")
    registry := djinn.Registry()

    greeter := &Greeter{}

    lease, _ := registry.Register(coordin8.Registration{
        Interface: "Greeter",
        Attrs: map[string]string{
            "language": "english",
        },
        TTL: 30 * time.Second,
        Handler: greeter,
    })
    defer lease.Cancel()

    // Auto-renew in background
    go lease.KeepAlive()

    fmt.Println("Serving...")
    select {} // block forever
}
```

**greeter_client/main.go:**

```go
package main

import (
    "fmt"
    "github.com/coordin8/sdk-go/coordin8"
)

func main() {
    djinn := coordin8.Connect("localhost:9002")
    registry := djinn.Registry()

    cap, err := registry.Lookup(coordin8.Template{
        "interface": "Greeter",
    })
    if err != nil {
        fmt.Println("No Greeter available")
        return
    }

    result, _ := cap.Invoke("Hello", "Coordin8")
    fmt.Println(result)
}
```

### Phase 4 — Polyglot Proof

**Goal:** Same demo, greeter in Go, client in Python. Proves polyglot works.

**Deliverables:**
- Python SDK: `pip install coordin8`
- Python greeter client calling Go greeter service through the Registry
- Same Djinn, same wire protocol, different languages

### Phase 5 — Space

**Goal:** Full Space implementation. `out()`, `take()`, `read()`, `watch()`.

**Deliverables:**
- `SpaceService` gRPC server in Rust
- Template matching (reuse engine from Registry)
- Lease support on tuples (TTL, expiry watches)
- Provenance metadata (auto-generated per tuple)
- Go + Python SDK support
- CLI: `coordin8 space list`, `coordin8 read`, `coordin8 out`, `coordin8 watch`
- IoT sensor mesh example

### Phase 6 — EventMgr

**Goal:** Durable event delivery. Store-and-forward mailbox semantics.

**Deliverables:**
- `EventService` gRPC server in Rust
- Subscribe with durable or best-effort delivery
- Leased subscriptions
- Any service can emit (EventSource trait)
- Go + Python SDK support

### Phase 7 — TransactionMgr

**Goal:** Real distributed transactions. Two-phase commit.

**Deliverables:**
- `TransactionService` gRPC server in Rust
- TransactionParticipant interface (protobuf + SDK)
- 2PC coordinator with write-ahead journal
- Transaction leases (TTL on txn, auto-abort on expiry)
- Same-provider optimization (single-store atomic when possible)
- Go + Python SDK support
- Cross-language transaction demo (Go + Python participants)

### Phase 8 — AWS Provider

**Goal:** Production-grade cloud backend.

**Deliverables:**
- DynamoDB for Space and Registry storage
- DynamoDB TTL for lease expiration
- SQS for durable event delivery
- EventBridge for watch() routing
- DynamoDB TransactWriteItems for same-provider txn optimization
- Terraform modules for all infrastructure
- LocalStack docker-compose for integration testing

### Phase 9 — Dashboard + Observability

**Goal:** Real-time visualization of the coordination plane.

**Deliverables:**
- TypeScript/React dashboard
- Live view of tuples flowing through spaces
- Registry service map
- Lease status and renewal timeline
- Transaction state visualization
- Provenance trace explorer (`trace <tuple-id>`)
- `coordin8 trace` CLI command

---

## Open Source Strategy

### Public repo contains:
- Core Djinn (Rust)
- CLI (Go)
- SDKs (Go, Python, Java/Kotlin, TypeScript)
- Local + AWS providers
- Terraform modules
- Dashboard
- Examples: IoT sensor mesh, distributed task queue

### Private repo (not open sourced):
- Trading platform built on Coordin8
- Signal connectors, Lenses, strategies
- GutLog correlation analysis
- Backtesting harness using Information pattern

### Public example: IoT Sensor Mesh
Authentic to origin story (Florida weather stations). Demonstrates every primitive without revealing the trading use case.

---

## Key Design Principles

1. **The network is an implementation detail.** No IPs, no ports, no endpoints in application code. Ever. The Djinn resolves everything.
2. **Time is a first-class concept.** Every operation has temporal semantics. TTL, timeout, lease. You MUST think about "how long is this fact valid."
3. **Absence is a signal.** Lease expiry is not cleanup — it's a coordination event. The thing that didn't happen is as important as the thing that did.
4. **The space never carries heavy data.** It carries knowledge about data — indexes, handles, capabilities. The Information pattern separates coordination plane from data plane.
5. **Transport is behind the capability.** Kafka, Kinesis, TCP, gRPC — invisible to the consumer unless they choose to filter on it.
6. **Same code, three tiers.** Local SQLite → LocalStack → Production cloud. One line config change.
7. **Transactions are real.** Not sagas. Not compensation. Two-phase commit with lease-based failure recovery. The TransactionParticipant interface is open to any service, not just spaces.
