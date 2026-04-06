# djinn

The Rust workspace that implements the Coordin8 daemon — the **Djinn**. Every coordination service (Lease, Registry, Proxy, Event, Transaction, Space) lives here as its own crate, plus the binary that wires them together at boot.

For the high-level pitch and architecture, see the [root README](../README.md) and [CLAUDE.md](../CLAUDE.md). This document is for working inside the workspace.

## Layout

```
djinn/
  Cargo.toml           workspace manifest (members + shared deps)
  crates/
    coordin8-core/     shared types + provider traits (LeaseStore, RegistryStore, ...)
    coordin8-proto/    tonic-generated gRPC bindings (build.rs compiles ../proto)
    coordin8-lease/    LeaseMgr + reaper                       (port 9001)
    coordin8-registry/ Registry + template matcher             (port 9002)
    coordin8-proxy/    Smart Proxy (TCP forwarder)             (port 9003)
    coordin8-txn/      TransactionMgr (2PC coordinator)        (port 9004)
    coordin8-event/    EventMgr (durable + best-effort)        (port 9005)
    coordin8-space/    Space (reactive tuple store)            (port 9006)
    coordin8-djinn/    binary entry point — boots all services
  providers/
    local/             InMemory provider (DashMap, default)
    dynamo/            DynamoDB provider (9 tables, MiniStack-tested)
```

## Boot Order

The binary in `crates/coordin8-djinn` enforces a strict layered boot. Violating this causes undefined behavior.

```
Layer 0   Provider               (storage backend)
Layer 1   LeaseMgr      :9001    (bedrock — everything depends on leases)
Layer 2a  Registry      :9002
Layer 2b  EventMgr      :9005    (subscribes to lease expiry)
Layer 2c  Space         :9006    (subscribes to lease expiry)
Layer 3   Proxy         :9003    (depends on Registry)
Layer 4   TransactionMgr :9004   (depends on LeaseMgr; lease expiry = abort)
```

Lease expirations are broadcast on a `tokio::sync::broadcast` channel; Registry, EventMgr, Space, and TransactionMgr each subscribe and act on resource-id prefixes (`registry:`, `event:`, `space:`, `space-watch:`, `txn:`).

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
