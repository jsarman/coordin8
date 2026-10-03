# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What Coordin8 Is

Coordin8 is a distributed coordination platform inspired by Jini/JavaSpaces (Sun Labs, 1999), decoupled from the JVM and modernized for any cloud. The wire protocol is gRPC + Protobuf. The core runtime — **the Djinn** — is written in Rust. Client SDKs exist for Go, Java, and Node.js/TypeScript.

**"Describe what you need, not where it is."**

The full vision (Space, EventMgr, TransactionMgr, AWS provider, higher-order patterns) is in `coordin8-design-napkin.md`.

> **Plans:** Work is tracked in `.claude/plans/<topic>/` — one folder per initiative (e.g. `djinn-split/`, `eventmgr/`, `hardening-roadmap/`), **committed to GitHub** (unlike `WORKFLOW.md`/blackboard below). Each topic folder has a `PRD.md` (goal, motivation, non-goals; a status line at the top — `Not started` / `In progress` / `COMPLETE (merged to main, <date>)` + PR link once shipped) and a `session-N-complete.md` per session that finished real work (narrative: what shipped, what was found, links to PRs/issues — a future session should be able to read it and know what happened in under a minute). Some topics also have `session-N-prep.md` (context queued for the next session) or `decisions.md` (specific design calls, each as Decision/Why/Trade-off — see `eventmgr/decisions.md`). `.claude/plans/PRD.md` (no subfolder) is the master index: one status table per architecture area, cross-referencing every topic folder — update its relevant table row(s) whenever a topic's status changes, and add a new topic folder for any initiative substantial enough to need its own goal/motivation/non-goals rather than just a table row.

> **Workflow:** If a `WORKFLOW.md` exists at the repo root, read it at session start. It contains developer-specific pacing and coordination preferences. This file is gitignored — each developer may have their own or none at all.

> **Blackboard:** `.claude/state/current.md` holds ephemeral session state — what's running, what's active, decisions in flight. Read it at session start for context. Write to it when the user says "save state" or at natural checkpoints. This file is gitignored.

## Architecture

### Djinn Services

Boot order is strict and load-bearing. No circular dependencies.

| Layer | Service | Port | Role |
|-------|---------|------|------|
| 0 | Provider | — | Storage backend (InMemory for dev) |
| 1 | Registry | 9002 | Attribute-based service discovery. Entries are leased — stop renewing, disappear. |
| 1 | EventMgr | 9005 | Durable event delivery. Leased subscriptions, mailbox buffering, sequence numbers. |
| 1 | Space | 9006 | Tuple store. `out/take/read/watch` with leased tuples and reactive streams. |
| 2 | Proxy | 9003 | TCP forwarding. `OpenProxy(template)` → local port with live failover. |
| 3 | TransactionMgr | 9004 | 2PC coordinator. Participants expose their own gRPC `ParticipantService`. |

**Providers are chosen at runtime.** `COORDIN8_PROVIDER=local` (default, InMemory) or `dynamo` (DynamoDB / MiniStack) via the `*_store_from_env()` factories in `coordin8-djinn/src/services.rs`, shared by bundled and split mode. Every store has two implementations (`djinn/providers/local`, `djinn/providers/dynamo`) that must behave identically. Each service's lease table is namespaced (`coordin8_leases_{registry,event,space,txn}`).

**Leasing is distributed, not a layer.** There is no standalone LeaseMgr service. Registry, EventMgr, Space, and TransactionMgr each embed their own `LeaseManager` and mount `LeaseService` on their own port — matching Jini/Apache River's `Landlord` pattern, where every service that grants leases manages them in-process rather than depending on a shared external service. This is why Registry, EventMgr, and Space all sit at Layer 1: none of them has a blocking dependency on anything else for their own operation. See `.claude/plans/distributed-leasing/PRD.md` for the full rationale (this replaced an earlier centralized-LeaseMgr design that turned out to be the root cause of a whole class of bootstrap-cycle bugs).

Lease TTL sentinels (`coordin8-core/src/lease.rs`): `LEASE_ANY = 0` (server picks), `LEASE_FOREVER = u64::MAX`; FOREVER `expires_at` is `DateTime::<Utc>::MAX_UTC`.

A lease is self-describing: the `Lease` message carries `grantor_host`/`grantor_port`, so a holder always knows where to renew without prior knowledge of which service granted it (mirrors Jini's `LandlordLease`).

### Split Mode

The default `djinn` binary boots all services in-process (bundled mode). Each service can also run as its own process via subcommands: `djinn registry | event | space | txn | proxy`. Split services discover each other through Registry — `COORDIN8_REGISTRY=host:9002` is the one well-known endpoint, used for self-registration and application-service discovery (unrelated to leasing, which each service handles itself).

`djinn healthcheck --addr http://host:port` probes a split service via the gRPC Health Checking Protocol (used by `docker-compose.split.yml`; bundled mode mounts no health service). Each split service binds `COORDIN8_BIND_ADDR`, advertises itself as `COORDIN8_ADVERTISE_HOST:<bound port>`, and self-registers with a 30s self-lease.

Two trait seams make this possible without touching manager internals:

- **`CapabilityResolver` trait** (`coordin8-core`) — abstracts Registry template resolution. `LocalCapabilityResolver` reads the in-process `RegistryStore` directly (bundled mode); `RemoteCapabilityResolver` (`coordin8-bootstrap`) forwards to a Registry gRPC client with transparent reconnect (split-mode Proxy).
- **`TxnEnlister` trait** (`coordin8-core`) — abstracts 2PC enlist. `LocalTxnEnlister` (`coordin8-txn`) calls directly into `TxnManager`; `RemoteTxnEnlister` (`coordin8-bootstrap`) discovers TxnMgr lazily through Registry (Space can boot before TxnMgr exists). Space auto-enlists on the first transactional write/take.

(The `Leasing` trait still exists in `coordin8-core`, but only to decouple downstream code from the concrete `LeaseManager` type — there's no remote/local split for it anymore, since every service's `LeaseManager` is always its own, in-process.)

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
  proto/coordin8/              .proto definitions (common, lease, registry, proxy, event, transaction, space)
  djinn/                       Rust workspace
    crates/
      coordin8-core/           Shared types, store traits, errors, lease sentinels
      coordin8-proto/          Generated gRPC bindings (tonic)
      coordin8-auth/           JWT auth (opt-in, COORDIN8_JWT_SECRET)
      coordin8-observability/  Structured logs, OTLP tracing, Prometheus metrics
      coordin8-lease/          LeaseManager + reaper (library — each service embeds its own)
      coordin8-registry/       Registry + template matcher
      coordin8-proxy/          ProxyManager + TCP forwarding
      coordin8-event/          EventMgr (durable delivery, mailbox, broadcast)
      coordin8-txn/            TransactionMgr (2PC coordinator)
      coordin8-space/          Space (tuples, watches, 2PC participant)
      coordin8-bootstrap/      self_register, RemoteCapabilityResolver, RemoteTxnEnlister
      coordin8-djinn/          Binary entry point (main.rs) + service boot (services.rs)
    providers/
      local/                   InMemory stores (coordin8-provider-local)
      dynamo/                  DynamoDB stores (coordin8-provider-dynamo; tests need MiniStack)
  sdks/
    go/coordin8/               Go SDK (client, lease, registry, proxy, discovery, event, space, auth)
    java/                      Java SDK (Gradle, gRPC stubs auto-generated)
    node/                      Node.js/TypeScript SDK (ts-proto generated)
  cli/cmd/coordin8/            Go CLI (Cobra) — lease, registry, space, auth (mint-token)
  examples/
    hello-coordin8/{go,java,node}/   Greeter service + clients
    market-watch/              EventMgr live demo — subscribe, mailbox drain, live stream
    double-entry/              TransactionMgr 2PC demo — happy path + veto abort
    auction-house/             Polyglot (Java + Go + Node) Space/EventMgr/Txn demo, own compose stack
  infra/                       dynamodb-tables.cfn.yml (12 DynamoDB tables)
  scripts/gen-auth-env.sh      Mint a JWT secret + per-identity tokens for the *-auth compose overlays
  Dockerfile.djinn / .greeter  Multi-stage builds
  docker-compose.yml           Bundled: MiniStack + cfn-init + Djinn + greeter
  docker-compose.split.yml     Split: registry/event/space/txn/proxy, one container each
  docker-compose.auth.yml / docker-compose.split.auth.yml   JWT overlays
  Makefile / .mise.toml        proto, build, build-examples, test, lint, clean; tool versions + tasks
  coordin8-design-napkin.md    Full vision / PRD
```

## Build Commands

```bash
# mise tasks (preferred — manages tool versions automatically)
mise r build             # Djinn release + CLI
mise r build-djinn       # Djinn dev build only
mise r build-examples    # all example binaries
mise r test              # cargo test --all + go test (Rust + Go SDK)
mise r test-rust         # Rust only (what CI runs)
mise r test-go           # Go SDK only
mise r lint              # clippy + fmt --check + go vet (SDK only; CI also vets cli/)
mise r proto             # regenerate all proto stubs
mise r clean             # remove all build artifacts
mise r djinn             # start Djinn locally (dev)
mise r up / down         # bundled Docker stack (MiniStack + Djinn + greeter)
mise r up-split / down-split  # split-mode Docker stack (registry/event/space/txn/proxy, one container each)
mise r up-auth / up-split-auth  # same stacks with JWT auth on (scripts/gen-auth-env.sh mints tokens)
mise r demo-events       # market-watch vs live Djinn on :9005
mise r demo-txn          # double-entry vs live Djinn on :9004
mise r demo-auction / demo-auction-down / demo-auction-auth  # Auction House polyglot demo (own Docker stack)
mise r demo-greeter-go / demo-greeter-java / demo-greeter-node  # hello-coordin8 clients (requires `up` or `djinn` running)

# Rust (from djinn/)
cargo build
cargo test
cargo test -p coordin8-lease       # single crate
cargo clippy --all --all-targets -- -D warnings   # stricter than CI (CI omits --all-targets)
cargo test -p coordin8-provider-dynamo -- --ignored   # Dynamo provider tests; needs MiniStack on :4566
cargo run                           # starts Djinn locally (in-memory)

# Go SDK (from sdks/go/) and CLI (from cli/)
go test -race ./...

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

### CI and test coverage

CI (`.github/workflows/ci.yml`) runs: Rust `cargo build --all`, `cargo test --all`, `cargo clippy --all -- -D warnings`, `cargo fmt --all --check` (Rust 1.94.1); a `dynamo-provider` job that runs the `#[ignore]`d Dynamo provider tests against a MiniStack service container; Go `build` + `vet` + `go test -race` for `cli/` and `sdks/go/`; `./gradlew build` (incl. JUnit tests) for the Java SDK; `npm run build && npm test` for the Node SDK. Locally, Dynamo tests need MiniStack on :4566 and `-- --ignored`. CI's clippy omits `--all-targets` — run it locally with `--all-targets` to lint test code too.

**Note:** The Makefile `GO ?=` falls back to `$(HOME)/go-install/go/bin/go` when mise is not active. With mise, `go` is on PATH and the env override takes effect automatically.

## Environment Variables

| Var | Used by | Meaning |
|-----|---------|---------|
| `COORDIN8_REGISTRY` | split services, examples | Registry `host:9002` — the one well-known endpoint |
| `COORDIN8_BIND_ADDR` | split services | Listen address (default `0.0.0.0:0`) |
| `COORDIN8_ADVERTISE_HOST` | Djinn services | Host peers dial (default `127.0.0.1`; container name in Docker). Go examples use `ADVERTISE_HOST` |
| `COORDIN8_PROVIDER` | Djinn | `local` (default) or `dynamo` |
| `DYNAMODB_ENDPOINT`, `AWS_*` | Dynamo provider | Endpoint override (MiniStack `http://localhost:4566`) + standard AWS creds/region |
| `COORDIN8_AUTO_CREATE_TABLES` | Dynamo provider | `true`/`1` creates tables on init (dev/tests); unset = expect CFN-provisioned |
| `MAX_LEASE_TTL`, `PREFERRED_LEASE_TTL` | LeaseManager | Seconds (max may be `FOREVER`); defaults 3600 / 300; per-service override `<REGISTRY\|EVENT\|SPACE\|TXN>_MAX_LEASE_TTL` etc. |
| `PROXY_BIND_HOST`, `PROXY_PORT_MIN/MAX` | Proxy | Forwarding bind address and fixed port range |
| `COORDIN8_JWT_SECRET` | all services | Enables gRPC JWT (HS256) auth; unset = auth off. `COORDIN8_AUTH_VERIFY_SIGNATURE=false` skips signature checks |
| `COORDIN8_TOKEN` | CLI | Bearer token (`coordin8 auth mint-token`; CLI also `--token`) |
| `COORDIN8_LOG_FORMAT` | all services | `json` for one-object-per-line logs (default pretty); level via `RUST_LOG` |
| `COORDIN8_OTEL_ENDPOINT`, `COORDIN8_OTEL_SAMPLE_RATIO` | all services | OTLP/gRPC trace export (unset = none); sample ratio default 1.0 |
| `COORDIN8_METRICS_PORT` | all services | Prometheus `/metrics` port (unset = no HTTP server) |

## Gotchas

### Proto

- `proxy.proto` uses `Release`, not `Close` — `close` conflicts with gRPC base `Client.close()` in Node's `@grpc/grpc-js`
- Java outer class for `lease.proto` is `LeaseOuterClass`, not `Lease` — protobuf renames the outer class when the filename collides with a message name. Import `coordin8.LeaseOuterClass.*`.
- Regenerate all stubs: `make proto`. Rust stubs auto-regenerate via `build.rs` on `cargo build`.

### Docker

- `PROXY_BIND_HOST=0.0.0.0` is required in containers — default `127.0.0.1` binds only inside the container
- Proxy port range `9100-9200` must be exposed in compose for host clients to reach forwarded ports
- `ADVERTISE_HOST` on services tells the Djinn proxy where to forward across the Docker bridge (e.g., `greeter` resolves to the greeter container)
- Health check: TCP probe on `:9002` (Registry — bundled mode doesn't mount the gRPC Health Checking Protocol on any port; only split mode's services do). Greeter `depends_on` this with `condition: service_healthy`.
- The base compose file starts MiniStack and a one-shot `cfn-init` (deploys all 12 tables) before Djinn, whatever `COORDIN8_PROVIDER` is; the default stays `local`.
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
- **Boot order is non-negotiable** — Provider → Registry/EventMgr/Space → Proxy → TransactionMgr. Violating this causes undefined behavior (though leasing itself is no longer part of this chain — each service embeds its own).
- **Two providers, one behavior** — any store change goes into both `providers/local` and `providers/dynamo`. Never compare `expires_at` as RFC3339 strings when FOREVER (`MAX_UTC`, formats as `+262142-...`) is possible; paginate every Dynamo Scan/Query.
- **The Space carries indexes and handles, never heavy data** — the Information pattern separates coordination from data plane
