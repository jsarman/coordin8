# Coordin8

> Describe what you need, not where it is.

Every cloud has durable, scalable building blocks — key-value stores, message queues, event buses, streams. What none of them have is coordination theory on top. Developers stitch together REST calls and end up with distributed monoliths: all the coupling, none of the transactional guarantees.

Coordin8 brings back the coordination model that Sun Labs got right in 1999 with Jini/JavaSpaces, decoupled from the JVM and modernized for any cloud. Same primitives. Modern runtime. Zero lock-in.

---

## What Coordin8 Does

### The Djinn

A Rust daemon that runs five layered coordination services:

| Layer | Service | Port | What it does |
|-------|---------|------|-------------|
| 0 | **Provider** | — | Pluggable storage backend (in-memory or DynamoDB) |
| 1 | **LeaseMgr** | 9001 | TTL-based liveness contracts. The bedrock — everything is leased. |
| 2a | **Registry** | 9002 | Attribute-based service discovery. Find services by *what they are*, not where they live. |
| 2b | **EventMgr** | 9005 | Durable event delivery with leased subscriptions. DURABLE (mailbox) and BEST_EFFORT modes. |
| 2c | **Space** | 9006 | Distributed reactive tuple store. `out`, `take`, `read`, `watch` with transactional isolation. |
| 3 | **Smart Proxy** | 9003 | TCP forwarding with live failover. `openProxy(template)` → local port. |
| 4 | **TransactionMgr** | 9004 | Real 2PC distributed transactions. Participants register their own gRPC endpoint. |

No circular dependencies. LeaseMgr is the foundation. Everything builds upward cleanly.

### Lease

A **liveness contract**: "this fact is true for a bounded period unless actively renewed." Stop renewing and the lease expires — that IS the signal.

Lease expiry drives leader election, distributed locks, circuit breakers, session management, and failure detection. Absence is not an error — it's a coordination event.

### Registry

Attribute-based service discovery. No DNS. No hardcoded endpoints. Ever.

```
# Service registers by what it is
registry.register(interface="Greeter", attrs={"language": "english"}, ttl=30s)

# Consumer finds it by capability, not location
greeter = registry.lookup({"interface": "Greeter"})
```

Stop renewing the lease → service disappears. No cleanup jobs. No stale entries.

### Smart Proxy

The proxy sits inside the Djinn and forwards TCP connections to discovered services. Consumers get a local port — no network details leak into application code. Each connection re-resolves the upstream at connect time, so failover is automatic.

### EventMgr

Durable event delivery modeled after the Jini Distributed Events Specification. Subscribe with a template and a TTL — get back a leased registration with a sequence number. Emit from any service. DURABLE mode buffers events in a per-subscription mailbox when no receiver is active. BEST_EFFORT mode is broadcast-only.

### TransactionMgr

Real 2PC distributed transactions modeled after `net.jini.core.transaction.server`. Participants run their own gRPC `ParticipantService` and enlist by endpoint. The coordinator runs prepare on all participants in parallel, then commits only if all vote PREPARED. NOTCHANGED voters are skipped at commit. Single-participant transactions use PrepareAndCommit to save a round-trip.

### Space

A distributed reactive tuple store inspired by JavaSpaces. Write tuples with `out`, read without removing with `read`, atomically remove with `take`, and subscribe to changes with `watch`. All operations support template matching. Transactional isolation ensures uncommitted writes are invisible to other clients and taken tuples are restored on abort.

### Provider

The provider layer is a set of storage traits (`LeaseStore`, `RegistryStore`, `EventStore`, `TxnStore`, `SpaceStore`) that abstract the backing store. Two implementations ship today:

- **`local`** (default) — in-memory with `DashMap`. Fast, zero dependencies, perfect for dev and edge.
- **`dynamo`** — DynamoDB-backed, 9 tables, tested against LocalStack. State survives restarts.

Switch at startup: `COORDIN8_PROVIDER=dynamo`

### Polyglot SDKs

All three SDKs have the same surface area:

| Language | Location | Build |
|----------|----------|-------|
| **Go** | `sdks/go/coordin8/` | `go build ./...` |
| **Java** | `sdks/java/` | `./gradlew build` |
| **Node.js/TypeScript** | `sdks/node/` | `npm run build` |

Each SDK provides: `DjinnClient`, `LeaseClient`, `RegistryClient`, `ProxyClient`, `SpaceClient`, and **`ServiceDiscovery`** — a Jini-inspired one-liner client that caches proxy connections by template and refreshes on lease expiry:

```go
// Go — one line to get a typed stub
discovery, _ := coordin8.Watch(djinn)
greeter := pb.NewGreeterClient(discovery.Get(coordin8.Template{"interface": "Greeter"}))
```

```java
// Java — same pattern
var discovery = ServiceDiscovery.watch(djinn);
var greeter = discovery.get(GreeterGrpc::newBlockingStub, Map.of("interface", "Greeter"));
```

```typescript
// Node — same pattern
const discovery = await ServiceDiscovery.watch(djinn);
const greeter = await discovery.get(addr => new GreeterClient(addr, creds), { interface: "Greeter" });
```

### CLI

Go CLI for inspecting and interacting with a running Djinn:

```bash
# Leases
coordin8 lease grant --resource worker-7 --ttl 30
coordin8 lease watch

# Registry
coordin8 registry list
coordin8 registry lookup --match interface=Greeter
coordin8 registry watch --match interface=WeatherStation
```

---

## Quick Start

### Prerequisites

- [mise](https://mise.jdx.dev) — manages Rust, Go, Node, Java versions (`mise install`)
- protoc + protoc-gen-go + protoc-gen-go-grpc (for proto regen only)
- Or: just Docker

### mise (recommended)

```bash
mise install       # install pinned tool versions
mise r djinn       # start Djinn locally
mise r test        # run all tests
mise r up          # start full Docker stack
```

### Docker (easiest full stack)

```bash
docker compose up --build
```

Starts the Djinn (ports 9001–9006, proxy range 9100–9200) and a Go greeter service pre-registered in the Registry.

Run a client against it:

```bash
# Go
cd examples/hello-coordin8/go && go run ./greeter_client/ John

# Java
cd examples/hello-coordin8/java && ./gradlew run --args="John"

# Node
cd examples/hello-coordin8/node && npx ts-node src/greeter-client.ts John
```

All three produce: `Hello, John! (from Coordin8 greeter_service)`

### Local

```bash
mise r build

# Terminal 1 — start the Djinn
mise r djinn
```

```
Djinn starting...
  ✓ Provider: local (in-memory)
  ✓ LeaseMgr:       listening on 0.0.0.0:9001
  ✓ Registry:       listening on 0.0.0.0:9002
  ✓ EventMgr:       listening on 0.0.0.0:9005
  ✓ Proxy:          listening on 0.0.0.0:9003
  ✓ TransactionMgr: listening on 0.0.0.0:9004
  ✓ Space:          listening on 0.0.0.0:9006
Djinn ready.
```

```bash
# Terminal 2 — start the greeter service
cd examples/hello-coordin8/go && go run ./greeter_service/

# Terminal 3 — call it
cd examples/hello-coordin8/go && go run ./greeter_client/ John
```

Kill the service. Run the client again:

```
Looking up Greeter in Registry...
  ✗ No Greeter available
```

No cleanup. No stale entries. The lease expired — that's the signal.

### With DynamoDB (LocalStack)

```bash
# Start with DynamoDB-backed persistence
COORDIN8_PROVIDER=dynamo docker compose up --build
```

All 9 DynamoDB tables are created automatically. State survives container restarts.

### Demos

```bash
# EventMgr — subscribe, emit offline, drain mailbox, emit live
mise r demo-events

# TransactionMgr — happy-path commit + veto abort proving no state change
mise r demo-txn
```

---

## Architecture

```
┌──────────────────────────────────────────────────────┐
│                  Application Code                    │
│  (no IPs, no ports, no hardcoded endpoints)          │
└───────────────┬──────────────────────────────────────┘
                │  gRPC + Protobuf
┌───────────────▼──────────────────────────────────────┐
│                  The Djinn (Rust)                     │
│                                                      │
│   LeaseMgr      :9001                                │
│   Registry      :9002                                │
│   EventMgr      :9005                                │
│   Space         :9006                                │
│   Proxy         :9003                                │
│   TransactionMgr :9004                               │
│                                                      │
│   Provider: local (in-memory) or dynamo (DynamoDB)   │
└──────────────────────────────────────────────────────┘
```

**Wire protocol:** gRPC + Protobuf. SDKs are thin wrappers over generated stubs — code generation handles the polyglot story.

**Template matching:** Services register with an interface name and key-value attributes. Consumers look up by template — partial match with operators (`contains:`, `starts_with:`, exact, or wildcard). No DNS. No service mesh. Just: "find me something that can do X."

---

## Repo Layout

```
coordin8/
  proto/coordin8/              Protobuf service definitions
  djinn/                       Rust workspace (core services)
    crates/
      coordin8-core/           Shared types and provider traits
      coordin8-proto/          Generated gRPC code
      coordin8-lease/          LeaseMgr implementation
      coordin8-registry/       Registry + template matching
      coordin8-proxy/          Smart Proxy (TCP forwarding)
      coordin8-event/          EventMgr implementation
      coordin8-txn/            TransactionMgr (2PC coordinator)
      coordin8-space/          Space (reactive tuple store)
      coordin8-djinn/          Binary entry point
    providers/
      local/                   In-memory provider (dev/edge)
      dynamo/                  DynamoDB provider (cloud/persistent)
  sdks/
    go/coordin8/               Go client SDK
    java/                      Java client SDK (Gradle)
    node/                      Node.js/TypeScript SDK (ts-proto)
  cli/                         Go CLI (coordin8)
  examples/
    hello-coordin8/
      go/                      Go greeter service + client
      java/                    Java greeter client
      node/                    Node greeter client
    market-watch/              EventMgr live demo (subscribe, mailbox, live stream)
    double-entry/              TransactionMgr 2PC demo (commit + veto abort)
  Dockerfile.djinn             Djinn container image
  Dockerfile.greeter           Greeter service container image
  docker-compose.yml           Full stack orchestration
  Makefile                     Build, test, lint, proto regen
  .mise.toml                   Tool versions + task runner
```

---

## Roadmap

| Service | Status | Description |
|---------|--------|-------------|
| **LeaseMgr** | Built | TTL-based liveness contracts |
| **Registry** | Built | Attribute-based service discovery |
| **Smart Proxy** | Built | TCP forwarding with live failover |
| **ServiceDiscovery** | Built | Jini-inspired one-liner client (Go, Java, Node) |
| **EventMgr** | Built | Durable event delivery, leased subscriptions, mailbox buffering |
| **TransactionMgr** | Built | Real 2PC — parallel prepare, single-participant optimization, veto abort |
| **Space** | Built | Distributed reactive tuple store with transactional isolation |
| **DynamoDB Provider** | Built | All 5 stores backed by DynamoDB (9 tables), tested against LocalStack |
| **Split Mode** | Built | Each service can run as its own process (`djinn lease\|registry\|event\|space\|txn\|proxy`), discoverable through Registry. Chaos-tested. |
| Cloud Topology | Planned | Independent service deployment — Lambda + Fargate + external pub/sub |
| Dashboard | Planned | Real-time coordination plane visualization |

See [coordin8-design-napkin.md](coordin8-design-napkin.md) for the full design — the five Djinn, higher-order patterns (Lens, Reflex, Sentry), the Information pattern, and the phased build plan.

---

## Origin

Coordin8 stands on the shoulders of Jini and JavaSpaces (Sun Microsystems, 1999). The coordination model — leased resources, attribute-based discovery, distributed transactions, reactive tuple spaces — was ahead of its time. The original implementation used Java RMI, which was the right choice for that era.

Coordin8 evolves those RMI-era concepts to use modern RPC (gRPC + Protobuf), a polyglot runtime (Rust core, Go/Java/Node SDKs), and cloud-native backing stores. Same principles, modern plumbing.

The cloud providers built the lamp. Coordin8 is the genie.

## License

Apache License 2.0 — see [LICENSE](LICENSE) for details.
