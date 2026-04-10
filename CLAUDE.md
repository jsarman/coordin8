# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What Coordin8 Is

Coordin8 is a distributed coordination platform inspired by Jini/JavaSpaces (Sun Labs, 1999), decoupled from the JVM and modernized for any cloud. The wire protocol is gRPC + Protobuf. The core runtime — **the Djinn** — is written in Rust. Client SDKs exist for Go, Java, and Node.js/TypeScript.

**"Describe what you need, not where it is."**

The full vision (Space, EventMgr, TransactionMgr, AWS provider, higher-order patterns) is in `coordin8-design-napkin.md`. Session plans live in `.claude/plans/`.

> **Workflow:** If a `WORKFLOW.md` exists at the repo root, read it at session start. It contains developer-specific pacing and coordination preferences. This file is gitignored — each developer may have their own or none at all.

> **Blackboard:** `.claude/state/current.md` holds ephemeral session state — what's running, what's active, decisions in flight. Read it at session start for context. Write to it when the user says "save state" or at natural checkpoints. This file is gitignored.

## Architecture

### Djinn Services

Boot order is strict and load-bearing. No circular dependencies.

| Layer | Service | Port | Role |
|-------|---------|------|------|
| 0 | Provider | — | Storage backend (InMemory for dev) |
| 1 | LeaseMgr | 9001 | TTL-based liveness contracts. Bedrock — everything depends on leases. |
| 2a | Registry | 9002 | Attribute-based service discovery. Entries are leased — stop renewing, disappear. |
| 2b | EventMgr | 9005 | Durable event delivery. Leased subscriptions, mailbox buffering, sequence numbers. |
| 2c | Space | 9006 | Tuple store. `out/take/read/watch` with leased tuples and reactive streams. |
| 3 | Proxy | 9003 | TCP forwarding. `OpenProxy(template)` → local port with live failover. |
| 4 | TransactionMgr | 9004 | 2PC coordinator. Participants expose their own gRPC `ParticipantService`. |

### Split Mode

The default `djinn` binary boots all services in-process (bundled mode). Each service can also run as its own process via subcommands: `djinn lease | registry | event | space | txn | proxy`. Split services discover each other through Registry — `COORDIN8_REGISTRY=host:9002` is the one well-known endpoint. Everything else self-registers under a short-TTL self-lease and is found by template lookup.

Three trait seams make this possible without touching manager internals:

- **`Leasing` trait** (`coordin8-core`) — abstracts lease operations. `LocalLeasing` wraps the in-process `LeaseManager`; `RemoteLeasing` (`coordin8-bootstrap`) wraps a gRPC client with transparent reconnect + retry on transport failure. Services written against the trait work identically in bundled and split mode.
- **`CapabilityResolver` trait** (`coordin8-core`) — abstracts Registry template resolution with the same local/remote split.
- **`TxnEnlister` trait** (`coordin8-core`) — abstracts 2PC enlist. `LocalTxnEnlister` (`coordin8-txn`) calls directly into `TxnManager`; `RemoteTxnEnlister` (`coordin8-bootstrap`) discovers TxnMgr lazily through Registry (Space can boot before TxnMgr exists). Space auto-enlists on the first transactional write/take.

Split mode survives LeaseMgr kills: a `RemoteLeasing` service whose current LeaseMgr dies re-resolves through Registry and continues against whatever instance is still up. Chaos tests in `djinn/crates/coordin8-djinn/tests/split_chaos.rs` enforce this.

### Smart Proxy

`ProxyManager` resolves a capability template against the Registry **at connection time**, not at open time. Each forwarded TCP connection re-resolves the upstream — if a service moves or fails over, the next connection finds the new one.

Configuration via env vars:
- `PROXY_BIND_HOST` — bind address (`127.0.0.1` locally, `0.0.0.0` in Docker)
- `PROXY_PORT_MIN` / `PROXY_PORT_MAX` — fixed port range (9100–9200 in compose); when unset, OS picks ephemeral ports

### ServiceDiscovery

Wraps the proxy layer with a cache keyed by template. Stale-on-lease-expire, eager-refresh-on-register. The one-liner pattern:

```go
// Go
discovery, _ := coordin8.Watch(djinn)
greeter := pb.NewGreeterClient(discovery.Get(coordin8.Template{"interface": "Greeter"}))
```

```java
// Java
var discovery = ServiceDiscovery.watch(djinn);
var greeter = discovery.get(GreeterGrpc::newBlockingStub, Map.of("interface", "Greeter"));
```

```typescript
// Node
const discovery = await ServiceDiscovery.watch(djinn);
const greeter = await discovery.get(addr => new GreeterClient(addr, creds), { interface: "Greeter" });
```

### Template Matching

Operators: `contains:`, `starts_with:`, exact match, or `Any` (missing field = match anything). The `interface` field is always injected into the match set alongside `attrs`.

## Repo Structure

```
coordin8/
  proto/coordin8/              .proto definitions (lease, registry, proxy, event, transaction, common)
  djinn/                       Rust workspace
    crates/
      coordin8-core/           Shared types, store traits
      coordin8-proto/          Generated gRPC bindings (tonic)
      coordin8-lease/          LeaseMgr + reaper
      coordin8-registry/       Registry + template matcher
      coordin8-proxy/          ProxyManager + TCP forwarding
      coordin8-event/          EventMgr (durable delivery, mailbox, broadcast)
      coordin8-txn/            TransactionMgr (2PC coordinator)
      coordin8-provider-local/ InMemory stores (lease, registry, event, txn)
      coordin8-djinn/          Binary entry point
  sdks/
    go/coordin8/               Go SDK (client, lease, registry, proxy, discovery)
    java/                      Java SDK (Gradle, gRPC stubs auto-generated)
    node/                      Node.js/TypeScript SDK (ts-proto generated)
  cli/cmd/coordin8/            Go CLI (Cobra) — lease + registry commands
  examples/
    hello-coordin8/
      go/                      Go greeter service + client
      java/                    Java greeter client (Gradle)
      node/                    Node.js greeter client (ts-node)
    market-watch/              EventMgr live demo — subscribe, mailbox drain, live stream
    double-entry/              TransactionMgr 2PC demo — happy path + veto abort
  Dockerfile.djinn             Multi-stage Rust build
  Dockerfile.greeter           Multi-stage Go build
  docker-compose.yml           Full stack: Djinn + greeter
  Makefile                     proto, build, build-examples, test, lint, clean
  .mise.toml                   Tool versions (rust, go, node, java) + task runner
  coordin8-design-napkin.md    Full vision / PRD
```

## Build Commands

```bash
# mise tasks (preferred — manages tool versions automatically)
mise r build             # Djinn release + CLI
mise r build-djinn       # Djinn dev build only
mise r build-examples    # all example binaries
mise r test              # Rust + Go SDK tests
mise r test-rust         # Rust only
mise r test-go           # Go SDK only
mise r lint              # clippy + fmt + go vet
mise r proto             # regenerate all proto stubs
mise r clean             # remove all build artifacts
mise r djinn             # start Djinn locally (dev)
mise r up / down         # Docker stack
mise r demo-events       # market-watch vs live Djinn on :9005
mise r demo-txn          # double-entry vs live Djinn on :9004

# Rust (from djinn/)
cargo build
cargo test
cargo test -p coordin8-lease       # single crate
cargo run                           # starts Djinn locally

# Go SDK (from sdks/go/)
go test ./...

# Node SDK (from sdks/node/)
npm install && npm run build
npm run proto                       # regenerate ts-proto stubs

# Node example (from examples/hello-coordin8/node/)
npx ts-node src/greeter-client.ts John

# Java SDK (from sdks/java/)
./gradlew build

# Java example (from examples/hello-coordin8/java/)
./gradlew run --args="John"

# Docker
docker compose up --build           # Djinn + greeter, full stack
docker compose down
```

**Note:** The Makefile `GO ?=` falls back to `$(HOME)/go-install/go/bin/go` when mise is not active. With mise, `go` is on PATH and the env override takes effect automatically.

## Gotchas

### Proto

- `proxy.proto` uses `Release`, not `Close` — `close` conflicts with gRPC base `Client.close()` in Node's `@grpc/grpc-js`
- Java outer class for `lease.proto` is `LeaseOuterClass`, not `Lease` — protobuf renames the outer class when the filename collides with a message name. Import `coordin8.LeaseOuterClass.*`.
- Regenerate all stubs: `make proto`. Rust stubs auto-regenerate via `build.rs` on `cargo build`.

### Docker

- `PROXY_BIND_HOST=0.0.0.0` is required in containers — default `127.0.0.1` binds only inside the container
- Proxy port range `9100-9200` must be exposed in compose for host clients to reach forwarded ports
- `ADVERTISE_HOST` on services tells the Djinn proxy where to forward across the Docker bridge (e.g., `greeter` resolves to the greeter container)
- Health check: TCP probe on `:9001` (LeaseMgr). Greeter `depends_on` this with `condition: service_healthy`.
- `restart: on-failure` on greeter handles DNS race at container startup
- `extra_hosts: host.docker.internal:host-gateway` on the djinn service — required on Linux so the 2PC coordinator can call back to participant servers running on the host. On Mac/Windows Docker Desktop this is automatic.

### Node SDK

- `tsconfig.json` has `rootDir: "."` — output lands in `dist/src/`, so `package.json` main/types point to `dist/src/index.{js,d.ts}`
- Stubs generated by `ts-proto` with `outputServices=grpc-js,esModuleInterop=true,env=node`
- `proxyClient` and `ServiceDiscovery.get` use a factory pattern `(address: string) => T` to avoid cross-module `@grpc/grpc-js` credential identity mismatches

### Java SDK

- Gradle: `proto { srcDir }` goes in the top-level `sourceSets {}` block, **not** inside the `protobuf {}` block (plugin 0.9.x doesn't support it there)
- Same `LeaseOuterClass` naming issue as above — affects all Java imports from `lease.proto`

## Design Constraints

- **Absence is a signal** — lease expiry is a coordination event, not an error. Don't catch it, handle it.
- **No IPs, ports, or endpoints in application code** — location is resolved through Registry + Proxy
- **Boot order is non-negotiable** — Provider → LeaseMgr → Registry/EventMgr → Proxy → TransactionMgr. Violating this causes undefined behavior.
- **The Space carries indexes and handles, never heavy data** — the Information pattern separates coordination from data plane
