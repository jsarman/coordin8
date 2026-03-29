# Coordin8

> Describe what you need, not where it is.

The cloud has durable, scalable building blocks — DynamoDB, SQS, EventBridge, Kafka, Kinesis. What it doesn't have is coordination theory on top. Developers stitch together REST calls and end up with distributed monoliths: all the coupling, none of the transactional guarantees.

Coordin8 brings back the coordination model that Sun Labs got right in 1999 with Jini/JavaSpaces, decoupled from the JVM and modernized for any cloud. Same primitives. Modern runtime. Zero lock-in.

---

## The Five Djinn

Five independent, composable services. Named after the Jini heritage (Jini → djinn/genie).

```
Layer 0:  Provider          SQLite (dev) → LocalStack → AWS
Layer 1:  LeaseMgr          Bedrock. Everything is leased.
Layer 2:  Space + EventMgr  Tuple store + durable event delivery
Layer 3:  TransactionMgr    Real 2PC — not sagas
Layer 4:  Registry          Attribute-based service discovery
```

No circular dependencies. LeaseMgr is the foundation. Everything builds upward cleanly.

### Lease

A **liveness contract**: "this fact is true for a bounded period unless actively renewed."

```
lease = grant(resource_id="worker-7", ttl=30s)
lease.renew(30s)   # still alive
lease.cancel()     # done

# Or just stop renewing — that IS the signal
```

Lease expiry drives leader election, distributed locks, circuit breakers, session management, and failure detection. Absence is not an error — it's a coordination event.

### Registry

Attribute-based service discovery. No DNS. No hardcoded endpoints. Ever.

```
# Service registers itself
lease = registry.register(
    interface="WeatherStation",
    attrs={"region": "tampa-east", "metrics": "wind,humidity,temp"},
    transport={"type": "grpc", "host": "sensor-7.internal", "port": "50051"},
    ttl=30s
)

# Consumer finds it by what it is, not where it lives
station = registry.lookup({"interface": "WeatherStation", "region": "tampa-east"})
stream = station.connect()   # no IP, no port in application code
```

Stop renewing the lease → service disappears from the Registry. No cleanup jobs. No stale entries.

### Space *(Phase 5)*

Named, distributed, reactive tuple store.

```
space.out({"order": "SOL-001", "status": "pending"})       # write
space.take({"status": "pending"})                           # atomic claim
space.watch({"status": "filled"}, handler=on_fill)         # reactive push
```

### EventMgr *(Phase 6)*

Durable event delivery. Store-and-forward mailbox semantics. Any service can emit. Subscriptions are leased.

### TransactionMgr *(Phase 7)*

Real distributed transactions across any participants. Two-phase commit with lease-based failure recovery. The transaction is itself leased — crash and it auto-aborts.

---

## Quick Start

### Prerequisites

- Rust (stable)
- Go 1.22+
- protoc + protoc-gen-go + protoc-gen-go-grpc

### Build

```bash
make build            # Djinn (Rust) + CLI (Go)
make build-examples   # hello-coordin8 demo binaries
```

### Run the Djinn

```bash
cd djinn
RUST_LOG=info ./target/release/djinn
```

```
Djinn starting...
  ✓ Provider: local (in-memory)
  ✓ LeaseMgr: listening on :9001
  ✓ Registry: listening on :9002
Djinn ready.
```

Add `RUST_LOG=coordin8_lease=debug,coordin8_registry=debug,djinn=debug` for full operation audit logs.

### Hello Coordin8

Two processes. Zero hardcoded addresses.

```bash
# Terminal 1 — start the greeter service
cd examples/hello-coordin8
bin/greeter-service

# Terminal 2 — find it and call it
bin/greeter-client John
```

```
Looking up Greeter in Registry...
  ✓ Found: Greeter  (capability=abc-123)
  ✓ Transport: grpc @ localhost:50051

  → Hello, John! (from Coordin8 greeter_service)
```

Kill the service. Run the client again:

```
Looking up Greeter in Registry...
  ✗ No Greeter available
```

No cleanup. No stale entries. The lease expired — that's the signal.

### CLI

```bash
cd djinn

# Leases
./coordin8 lease grant --resource worker-7 --ttl 30
./coordin8 lease renew --id <lease_id> --ttl 30
./coordin8 lease cancel --id <lease_id>
./coordin8 lease watch                         # stream expiry events

# Registry
./coordin8 registry register --interface WeatherStation \
  --attr region=tampa-east --attr metrics=wind,humidity,temp \
  --transport grpc --host sensor-7.internal --ttl 60
./coordin8 registry list
./coordin8 registry lookup --match interface=WeatherStation --match region=tampa-east
./coordin8 registry watch --match interface=WeatherStation
```

---

## Architecture

```
┌──────────────────────────────────────────────────────┐
│                  Application Code                    │
│  (no IPs, no ports, no hardcoded endpoints)          │
└───────────────┬──────────────────────────────────────┘
                │  gRPC  (Protobuf)
┌───────────────▼──────────────────────────────────────┐
│                  The Djinn (Rust)                    │
│                                                      │
│   LeaseMgr  ·  Space  ·  EventMgr                   │
│   TransactionMgr  ·  Registry                        │
│                                                      │
│   Provider: SQLite | DynamoDB | CosmosDB             │
└──────────────────────────────────────────────────────┘
```

**Wire protocol:** gRPC + Protobuf. SDKs are thin wrappers — code generation handles the rest.

**Provider tiers:**

```
Dev:          SQLite in-process  (zero dependencies)
Integration:  LocalStack on Docker Desktop
Production:   AWS (DynamoDB, SQS, EventBridge)
```

Same application code across all three. One config line difference.

---

## Repo Layout

```
coordin8/
  proto/coordin8/       Protobuf service definitions
  djinn/                Rust workspace (core services)
    crates/
      coordin8-core     Shared types and provider traits
      coordin8-proto    Generated gRPC code
      coordin8-lease    LeaseMgr implementation
      coordin8-registry Registry implementation
      coordin8-djinn    Binary — boots all services
    providers/
      local/            In-memory provider (dev)
  cli/                  Go CLI (coordin8)
  sdks/go/              Go client SDK
  examples/
    hello-coordin8/     End-to-end demo (Phase 3)
```

---

## Build Phases

| Phase | Status | Description |
|-------|--------|-------------|
| 0 | ✅ | Foundation — Rust workspace, proto defs, CI |
| 1 | ✅ | LeaseMgr — gRPC server, Go SDK, Go CLI |
| 2 | ✅ | Registry — attribute-based discovery, template matching |
| 3 | ✅ | Hello Coordin8 — end-to-end demo, zero hardcoded addresses |
| 4 | — | Polyglot proof — Python or Java client, same Djinn |
| 5 | — | Space — tuple store with reactive watch |
| 6 | — | EventMgr — durable event delivery |
| 7 | — | TransactionMgr — real 2PC |
| 8 | — | AWS provider — DynamoDB, SQS, EventBridge |
| 9 | — | Dashboard + observability |

---

## Origin

Built on the shoulders of Jini/JavaSpaces (Sun Labs, 1999). The coordination model was right. The JVM dependency and multicast discovery were not. Coordin8 keeps the primitives, replaces the plumbing.

The cloud providers built the lamp. Coordin8 is the genie.
