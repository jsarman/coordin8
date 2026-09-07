# djinn

The Rust workspace that implements the Coordin8 daemon — the **Djinn**. Every coordination service (Registry, Proxy, Event, Transaction, Space) lives here as its own crate, plus the binary that wires them together at boot. Leasing isn't a separate service — it's a library (`coordin8-lease`) that Registry, EventMgr, Space, and TransactionMgr each embed.

For the high-level pitch and architecture, see the [root README](../README.md) and [CLAUDE.md](../CLAUDE.md). This document is for working inside the workspace.

## Layout

```
djinn/
  Cargo.toml           workspace manifest (members + shared deps)
  crates/
    coordin8-core/     shared types + provider traits (LeaseStore, RegistryStore, ...)
    coordin8-proto/    tonic-generated gRPC bindings (build.rs compiles ../proto)
    coordin8-lease/    LeaseManager + reaper (library — no port of its own; Registry/EventMgr/Space/TransactionMgr each embed one)
    coordin8-registry/ Registry + template matcher             (port 9002, + LeaseService)
    coordin8-proxy/    Smart Proxy (TCP forwarder)             (port 9003)
    coordin8-txn/      TransactionMgr (2PC coordinator)        (port 9004, + LeaseService)
    coordin8-event/    EventMgr (durable + best-effort)        (port 9005, + LeaseService)
    coordin8-space/    Space (reactive tuple store)            (port 9006, + LeaseService)
    coordin8-djinn/    binary entry point — boots all services
  providers/
    local/             InMemory provider (DashMap, default)
    dynamo/            DynamoDB provider (12 tables, MiniStack-tested)
```

## Boot Order

The binary in `crates/coordin8-djinn` enforces a strict layered boot. Violating this causes undefined behavior.

```
Layer 0   Provider               (storage backend)
Layer 1   Registry      :9002    (+ LeaseService — no blocking dependency on anything else)
Layer 1   EventMgr      :9005    (+ LeaseService — no blocking dependency on anything else)
Layer 1   Space         :9006    (+ LeaseService — no blocking dependency on anything else)
Layer 2   Proxy         :9003    (depends on Registry)
Layer 3   TransactionMgr :9004   (+ LeaseService; lease expiry = abort)
```

Leasing is distributed, not a layer of its own: there's no standalone LeaseMgr — Registry, EventMgr, Space, and TransactionMgr each embed their own `LeaseManager` and mount `LeaseService` on their own port, matching Jini/Apache River's `Landlord` pattern. That's why Registry, EventMgr, and Space all sit at Layer 1 — none of them has a blocking dependency on anything else for its own operation. See [`.claude/plans/distributed-leasing/PRD.md`](../.claude/plans/distributed-leasing/PRD.md) for the full rationale.

Each service's own lease expirations are broadcast on its own `tokio::sync::broadcast` channel; Registry, EventMgr, Space, and TransactionMgr each subscribe to their own and act on resource-id prefixes (`registry:`, `event:`, `space:`, `space-watch:`, `txn:`).

## Provider Selection

```bash
COORDIN8_PROVIDER=local    # default — in-memory, dev/edge
COORDIN8_PROVIDER=dynamo   # DynamoDB-backed
```

When `dynamo` is selected, the binary calls `init()` on each store. If `COORDIN8_AUTO_CREATE_TABLES=true` is set, the stores will create their tables; otherwise they assume the tables already exist (deployed by CloudFormation — see [`../infra/`](../infra/README.md)).

## Build / Test / Run

```bash
# from this directory
cargo build                      # dev build of every crate
cargo build --release            # release build (used by Dockerfile.djinn)
cargo test --all                 # all crate tests
cargo test -p coordin8-lease     # single crate (any workspace member)
cargo run                        # boot Djinn locally (default: local provider)
cargo clippy --all -- -D warnings
cargo fmt --all
```

From the repo root the same operations are available via `mise`:

```bash
mise r build-djinn   # cargo build
mise r test-rust     # cargo test --all
mise r djinn         # cargo run
mise r lint          # clippy + fmt + go vet
```

## Adding a New Crate

1. `cargo new --lib crates/coordin8-foo`
2. Add it to `members` in `Cargo.toml` and to `[workspace.dependencies]`
3. Depend on `coordin8-core` for shared types and `coordin8-proto` for gRPC bindings
4. Wire it into `crates/coordin8-djinn/src/main.rs` in the correct layer

## Gotchas

- `coordin8-proto` regenerates stubs via `build.rs` on every change to `../proto/coordin8/*.proto` — no manual step needed
- The `coordin8-djinn` binary is the **only** member that pulls in both providers; library crates depend on traits from `coordin8-core`, never on a concrete provider
- All servers bind to `0.0.0.0` — set firewall / Docker port mapping accordingly
