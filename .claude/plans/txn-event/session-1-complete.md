# Session 1 Complete — EventMgr + TransactionMgr

## What Was Built

### EventMgr (Layer 2b, port 9005)
- `coordin8-event` Rust crate — EventManager + EventServiceImpl
- `InMemoryEventStore` — DashMap subscriptions + VecDeque mailbox per subscription
- DURABLE / BEST_EFFORT delivery modes
- Per-(source, event_type) atomic sequence counters
- Receive stream: subscribes to broadcast BEFORE draining mailbox (race-free; overlap handled by seq_num on client)
- Template matching reuses `coordin8_registry::matcher`
- Go proto stubs generated: `sdks/go/gen/coordin8/event*.go`
- Live demo: `examples/market-watch/` — subscribe, emit offline, drain mailbox, emit live, cancel

### TransactionMgr (Layer 4, port 9004)
- `coordin8-txn` Rust crate — TxnManager + TxnServiceImpl
- Full 2PC state machine: ACTIVE → VOTING → PREPARED → COMMITTED (or ABORTED)
- Parallel prepare via `join_all` — any VOTE_ABORTED aborts all
- Parallel commit via `join_all` — NOTCHANGED voters excluded
- Single-participant PrepareAndCommit optimization (saves round-trip)
- Zero-participant trivial commit
- Outbound gRPC via `Channel::from_shared` per call (no pool — v1)
- Lease on every transaction — expiry = auto-abort
- Go proto stubs generated: `sdks/go/gen/coordin8/transaction*.go`
- Live demo: `examples/double-entry/` — happy-path commit + veto abort proving no state change

### Examples Reorganization
- Moved `hello-coordin8/{go,java,node}` into `examples/hello-coordin8/<lang>/`
- Fixed all relative paths (go.mod, settings.gradle, build.gradle, package.json, Dockerfile.greeter, Makefile)
- `event_demo` extracted from hello-coordin8/go → `examples/market-watch/`
- Created `examples/double-entry/`

### Infrastructure
- `mise.toml` added — pins rust 1.93, go 1.22, node 23, java openjdk-21; task runner for all workflows
- `extra_hosts: host.docker.internal:host-gateway` on djinn — Linux Docker 2PC callback fix
- `resolveAdvertiseHost()` in double-entry — DNS probe selects `host.docker.internal` (Docker) or `127.0.0.1` (local) automatically
- Cleaned unused import + dead field warnings in `coordin8-proxy`

## Key Decisions

**EventMgr independent of Space** — uses InMemory provider directly, same pattern as LeaseMgr. Space comes later.

**TransactionMgr independent of Space** — same rationale.

**Receive stream race fix** — broadcast subscription opened BEFORE mailbox drain. Any events emitted in the gap show up on the live stream. Client deduplicates by seq_num if needed.

**PrepareAndCommit optimization** — single-participant case skips a round-trip. Triggered when `participants.len() == 1` in TxnManager::commit().

**2PC post-commit failure handling** — if commit phase fails after prepare succeeds, participants are left committed (2PC guarantee — coordinator cannot roll back a committed participant). Failures logged as WARN.

**Enum namespace collision** — `proto/coordin8/transaction.proto` originally had `PREPARED` and `ABORTED` in both TransactionState and PrepareVote. C++ flat enum scoping caused protoc to fail. Fixed by prefixing PrepareVote values: `VOTE_PREPARED`, `VOTE_NOTCHANGED`, `VOTE_ABORTED`.

## Gaps Left Open

- EventMgr SDK: Java, Node clients not built
- TransactionMgr SDK: Java, Node clients not built
- Same-provider optimization (DynamoDB TransactWriteItems) — Phase 8
- Connection pooling for outbound participant gRPC calls — v2
- EventMgr O(n) emit (scans all subscriptions) — acceptable now, index by source in v2

## Tests

- `coordin8-event/tests/demo.rs` — 5 in-process tests: mailbox drain, best-effort, template filtering, seq numbers, cancel
- `coordin8-txn/tests/demo.rs` — 6 tests with real tonic participant servers on random ports: single-participant, multi-participant, veto abort, explicit abort, NOTCHANGED skipped, zero-participant
