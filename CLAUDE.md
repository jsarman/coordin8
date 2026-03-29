# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project State

**Pre-alpha, design phase.** The single source of truth is `coordin8-design-napkin.md` — read it before touching anything. No code exists yet. Phase 0 (Rust workspace + protobuf scaffolding) is the starting point.

## Planned Repo Structure

```
coordin8/
  proto/coordin8/           ← .proto service definitions (lease, registry, common)
  djinn/                    ← Rust workspace (core services)
    Cargo.toml
    crates/
      coordin8-proto/       ← generated protobuf/gRPC code
      coordin8-core/        ← shared types, provider trait
      coordin8-lease/       ← LeaseMgr implementation
      coordin8-registry/    ← Registry (Space with leased entries)
      coordin8-space/       ← Space implementation
      coordin8-event/       ← EventMgr
      coordin8-txn/         ← TransactionMgr
      coordin8-djinn/       ← binary, bootstraps all services
    providers/local/        ← SQLite/in-memory (dev tier)
  cli/                      ← Go module
  sdks/go/                  ← Go client SDK
  examples/hello-coordin8/
```

## Build Commands (Phase 0 onward)

```bash
# Rust — core Djinn
cargo build
cargo test
cargo test -p coordin8-lease         # single crate
cargo clippy -- -D warnings
cargo fmt --check

# Protobuf
protoc --rust_out=. --grpc_out=. proto/coordin8/*.proto

# Go — CLI + SDKs
go build ./cli/...
go test ./...
go vet ./...
gofmt -l .
```

## Architecture — The Five Djinn

Boot order is strict (no circular deps):

```
Layer 0: Provider          (DynamoDB | SQLite | CosmosDB | Firestore)
Layer 1: LeaseMgr          ← bedrock; everything depends on leases
Layer 2: Space + EventMgr  ← peers, no dependency between them
Layer 3: TransactionMgr    ← uses LeaseMgr + provider
Layer 4: Registry          ← implemented as a Space with leased entries
```

**LeaseMgr** — TTL-based liveness contracts. Lease expiry is a coordination signal, not an error. Drives leader election, distributed locks, session management, failure detection.

**Space** — Named tuple store. Primitives: `out()` (write), `take()` (atomic claim+remove), `read()` (non-destructive), `watch()` (reactive push). Template matching: null fields match anything. Every op takes optional tx, TTL, lease.

**EventMgr** — Independent of Space. Delivery modes: durable (store-and-forward) or best-effort. Leased subscriptions.

**TransactionMgr** — Real 2PC distributed transactions (not sagas). Same-provider path uses native atomic ops (e.g., DynamoDB `TransactWriteItems`). Transactions are leased — auto-abort on expiry.

**Registry** — Attribute-based service discovery (not DNS). Pure pattern: it's a Space with leased entries. Consumers match on capability templates, not hardcoded endpoints.

## Provider Tiers (Three Environments, One Codebase)

```python
Djinn(provider="local")                                   # SQLite in-process (dev)
Djinn(provider="aws", endpoint="http://localhost:4566")   # LocalStack (integration)
Djinn(provider="aws", region="us-east-1")                 # Production
```

## Wire Protocol

gRPC + Protobuf. Capabilities (smart proxies) are descriptor-based — no executable code crosses the wire; the SDK hydrates the proxy locally.

## Build Phases

0. Foundation: Rust workspace, proto defs, CI
1. LeaseMgr
2. Registry
3. Hello Coordin8 (end-to-end demo, Go example)
4. Polyglot SDK proof (Go + one other)
5. Space
6. EventMgr
7. TransactionMgr
8. AWS provider
9. Dashboard + observability

## Key Design Constraints

- Absence is a signal — don't treat lease expiry as an exception, handle it as a coordination event
- The Space carries indexes and handles, never heavy data (Information pattern separates coordination from data plane)
- No IPs, ports, or endpoints in application code — location is resolved through Registry
- No circular dependencies between the five Djinn layers — the boot order is load-bearing
