# Coordin8 — Master PRD

> Single source of truth for what's built, what's not, and what's next.
> Update this file as work completes. See `coordin8-design-napkin.md` for the full vision.

---

## LeaseMgr

The bedrock. TTL-based liveness contracts.

| Item | Status | Notes |
|------|--------|-------|
| Proto: Grant, Renew, Cancel, WatchExpiry | Done | `proto/coordin8/lease.proto` |
| Rust: LeaseManager + reaper | Done | `coordin8-lease` crate, 1s reaper interval |
| Duration negotiation + `FOREVER` / `ANY` constants | Done | `MAX_LEASE_TTL` / `PREFERRED_LEASE_TTL` env vars |
| Go SDK: grant, renew, cancel, keepAlive, watch | Done | Full parity |
| Java SDK: grant, renew, cancel | Done | |
| Java SDK: keepAlive (background renewal) | **Gap** | Go has it, Java doesn't |
| Java SDK: watch (expiry stream) | **Gap** | Go has it, Java doesn't |
| Node SDK: grant, renew, cancel, keepAlive, watch | Done | |
| CLI: grant, renew, cancel, watch | Done | |

---

## Registry

Attribute-based service discovery with leased entries.

| Item | Status | Notes |
|------|--------|-------|
| Proto: Register, Lookup, LookupAll, Watch, ModifyAttrs | Done | `proto/coordin8/registry.proto` |
| Rust: RegistryIndex + matcher | Done | `coordin8-registry` crate |
| Template ops: exact, contains:, starts_with:, Any | Done | |
| Template ops: Gte, Lte, Range | **Gap** | Napkin mentions them, not implemented |
| TransportDescriptor in Capability | Done | type + config map |
| ServiceID-style re-registration (`capability_id`) | Done | `RegisterResponse` carries cap_id + lease |
| `ModifyAttrs` RPC + `MODIFIED` event type | Done | Jini modifyAttributes pattern |
| Lease-expiry cascade (zombie entry cleanup) | Done | Phase 2 critical fix |
| Go SDK: register, lookup, lookupAll, watch, modifyAttrs | Done | Full parity |
| Java SDK: register, lookup, lookupAll, watch, modifyAttrs | Done | Added in Phase 2 |
| Node SDK: register, lookup, lookupAll, watch, modifyAttrs | Done | Added in Phase 2 |
| CLI: list, register, lookup, watch | Done | |

---

## Smart Proxy

TCP forwarding inside the Djinn. `openProxy(template)` → local port.

| Item | Status | Notes |
|------|--------|-------|
| Proto: Open, Release | Done | `proxy.proto` — Release, not Close (Node conflict) |
| Rust: ProxyManager + TCP forwarding | Done | `coordin8-proxy` crate |
| ProxyConfig (bind host, port range) | Done | Env vars: PROXY_BIND_HOST, PORT_MIN, PORT_MAX |
| Re-resolve upstream per connection | Done | Live failover at connect time |
| Go SDK: open, release, ProxyConn | Done | |
| Java SDK: open, release, proxyStub | Done | |
| Node SDK: open, release, proxyClient | Done | Factory pattern for grpc-js compat |
| CLI: proxy commands | **Gap** | No CLI for proxy (accessed through Registry) |

---

## ServiceDiscovery

Jini-inspired one-liner client. Caches proxies by template.

| Item | Status | Notes |
|------|--------|-------|
| Go: Watch, Get, Close | Done | Watch-based refresh working |
| Go: stale-on-expire, eager-refresh | Done | Background goroutine watches registry |
| Java: watch, get, close | Done | Cache works |
| Java: watch-based refresh | Done | stale-on-expire, refresh-on-register, refresh-on-modified |
| Node: watch, get, close | Done | Cache works |
| Node: watch-based refresh | Done | stale-on-expire, refresh-on-register, refresh-on-modified |

---

## Space (Tuple Store)

Distributed reactive coordination store. `out/take/read/watch`. See `.claude/plans/space/` for full detail.

| Item | Status | Notes |
|------|--------|-------|
| Proto: SpaceService definition | Done | `proto/coordin8/space.proto` — Jini-named RPCs |
| Rust: Space crate | Done | `coordin8-space` — boots Layer 2c, port 9006 |
| Write (was Out) — attrs, payload, ttl, written_by, lineage | Done | Leased, broadcasts to wake blockers |
| Take (template, wait, timeout_ms) | Done | Atomic claim+remove, blocking with race handling |
| Read (template, wait, timeout_ms) | Done | Non-destructive, blocking or non-blocking |
| Notify (was Watch) — template, handback, ttl | Done | Server-streaming, appearance + expiration, Jini handback |
| Contents (bulk read streaming) | Done | JavaSpace05.contents — added in Phase 2 |
| Renew / Cancel (was RenewTuple/CancelTuple) | Done | Lease management on stored tuples |
| TTL / lease on tuples + notifies | Done | Reaper integration via expiry_tx listener |
| Provenance metadata | Done | written_by, written_at, input_tuple_id (lineage) |
| Template matching | Done | Reuses `coordin8_registry::matcher` |
| InMemorySpaceStore | Done | Race-safe take_match, lease_index for reaper |
| Integration tests | Done | 13 Rust tests — blocking, race, expiry, template ops |
| `txn_id` field reserved on Write/Read/Take/Notify/Contents | Done | Wire-compatible; server currently ignores |
| Transaction isolation (Space as 2PC participant) | Done | `TxnEnlister` trait seam, auto-enlist on write/take, `LocalTxnEnlister` + `RemoteTxnEnlister`, 13 unit + 2 split integration tests |
| Go SDK | Done | `SpaceClient` — Write, Read, Take, Contents, Notify, RenewTuple, CancelTuple |
| Java SDK | **Gap** | Generated stubs present, no hand-written client |
| Node SDK | **Gap** | Generated stubs present, no hand-written client |
| CLI: spaces list, read, out, watch | **Gap** | Blocked on Java/Node SDK |

---

## EventMgr

Durable event delivery. Store-and-forward mailbox semantics. Independent of Space.

| Item | Status | Notes |
|------|--------|-------|
| Proto: EventService definition | Done | `proto/coordin8/event.proto` |
| Rust: EventMgr crate | Done | `coordin8-event` — boots Layer 2b alongside Registry |
| Subscribe → EventRegistration (id + lease + seqNum) | Done | Jini EventRegistration pattern |
| Receive (server-streaming) | Done | Subscribe to broadcast BEFORE draining mailbox (race-free) |
| Emit (any service can call) | Done | Broadcasts + enqueues to all matching durable subscriptions |
| RenewSubscription / CancelSubscription | Done | |
| Subscription lease-expiry cascade | Done | Phase 2 critical fix — `remove_by_lease` + `unsubscribe_by_lease` |
| DeliveryMode: DURABLE vs BEST_EFFORT | Done | DURABLE = mailbox; BEST_EFFORT = broadcast only |
| Sequence numbers on Event (gap detection) | Done | Per (source, event_type) atomic counter |
| Handback bytes (echoed from subscribe) | Done | Jini handback pattern |
| InMemory subscription + mailbox store | Done | `InMemoryEventStore` — DashMap + VecDeque per subscription |
| Template matching on emit routing | Done | Reuses `coordin8_registry::matcher` |
| Go SDK proto stubs | Done | `sdks/go/gen/coordin8/event*.go` |
| Go live demo | Done | `examples/market-watch/` |
| Java SDK | **Gap** | |
| Node SDK | **Gap** | |

---

## TransactionMgr

Real 2PC distributed transactions. Not sagas. Independent of Space — uses InMemory provider directly, same pattern as LeaseMgr.

| Item | Status | Notes |
|------|--------|-------|
| Proto: TransactionService + ParticipantService | Done | `proto/coordin8/transaction.proto` |
| Rust: TransactionMgr crate | Done | `coordin8-txn` — boots Layer 4, port 9004 |
| Begin → TransactionCreated (id + lease) | Done | Lease expiry = auto-abort |
| Enlist (participant registers endpoint + crashCount) | Done | Jini join() pattern |
| GetState | Done | ACTIVE / VOTING / PREPARED / COMMITTED / ABORTED |
| Commit (parallel prepare, then parallel commit) | Done | `join_all` for both phases |
| Abort (calls abort on all participants) | Done | |
| PrepareAndCommit optimization (single participant) | Done | Skips extra round-trip |
| NOTCHANGED voters skipped at commit phase | Done | Read-only participant optimization |
| Transaction lease-expiry auto-abort cascade | Done | Phase 2 critical fix — `abort_expired()` wired in main.rs |
| ParticipantService: Prepare, Commit, Abort, PrepareAndCommit | Done | Participants run own gRPC server |
| InMemory transaction journal | Done | `InMemoryTxnStore` — participants embedded in TransactionRecord |
| host.docker.internal for callback routing | Done | `extra_hosts: host-gateway` in compose |
| Go SDK proto stubs | Done | `sdks/go/gen/coordin8/transaction*.go` |
| Go live demo | Done | `examples/double-entry/` — happy path + veto abort |
| Same-provider optimization | **Gap** | DynamoDB TransactWriteItems — Phase 8 |
| Java SDK | **Gap** | |
| Node SDK | **Gap** | |

---

## Providers

Pluggable storage backends.

| Item | Status | Notes |
|------|--------|-------|
| InMemory (dev) | Done | `coordin8-provider-local` crate |
| InMemoryLeaseStore | Done | |
| InMemoryRegistryStore | Done | |
| InMemoryEventStore | Done | DashMap + VecDeque mailbox per subscription |
| InMemoryTxnStore | Done | Participants embedded in TransactionRecord |
| InMemorySpaceStore | Done | DashMap + lease_index, race-safe take_match |
| SQLite (local persistence) | Not started | Napkin mentions, not prioritized |
| AWS: DynamoDB | Not started | Phase 8 — primary prod provider |
| AWS: SQS (durable events) | Not started | Phase 8 |
| AWS: EventBridge (watch routing) | Not started | Phase 8 |
| Terraform modules | Not started | |

---

## Docker & Orchestration

| Item | Status | Notes |
|------|--------|-------|
| Dockerfile.djinn | Done | Multi-stage Rust build, protoc v28.3 |
| Dockerfile.greeter | Done | Multi-stage Go build |
| docker-compose.yml | Done | Djinn + greeter, ports 9001–9006 + proxy range |
| host.docker.internal | Done | `extra_hosts: host-gateway` on djinn — 2PC callback routing |
| .dockerignore | Done | |
| MiniStack service | Stubbed | Commented out in compose, ready to activate |
| Dashboard service | Stubbed | Commented out in compose |

---

## CI/CD

| Item | Status | Notes |
|------|--------|-------|
| GitHub Actions: Rust (build, test, clippy, fmt) | Done | `.github/workflows/ci.yml` |
| GitHub Actions: Go CLI (build, vet) | Done | |
| GitHub Actions: Go SDK (build, vet) | Done | |
| GitHub Actions: Java (Gradle build) | **Gap** | |
| GitHub Actions: Node (npm build, test) | **Gap** | |
| Integration tests (compose + examples) | **Gap** | |

---

## Examples

| Item | Status | Notes |
|------|--------|-------|
| Go greeter service | Done | `examples/hello-coordin8/go/greeter_service/` |
| Go greeter client | Done | `examples/hello-coordin8/go/greeter_client/` |
| Java greeter client | Done | `examples/hello-coordin8/java/` |
| Node greeter client | Done | `examples/hello-coordin8/node/` |
| market-watch (EventMgr demo) | Done | `examples/market-watch/` — subscribe, mailbox drain, live stream |
| double-entry (TransactionMgr demo) | Done | `examples/double-entry/` — happy-path + veto abort |
| Node greeter service | Not started | Would complete polyglot service story |
| Java greeter service | Not started | |
| IoT sensor mesh | Not started | Napkin showcase example |
| Distributed task queue | Not started | Napkin showcase example |

---

## CLI

| Item | Status | Notes |
|------|--------|-------|
| lease grant/renew/cancel/watch | Done | |
| registry list/register/lookup/watch | Done | |
| proxy commands | Not started | Low priority — accessed via SDK |
| spaces list/read/out/watch | Not started | Blocked on Space SDK |
| trace \<tuple-id\> | Not started | Blocked on provenance |

---

## Phase 2: Spec Alignment

Bottom-up alignment against the Jini specification. See `.claude/plans/phase2-spec-alignment/`.

| Session | Status | Notes |
|---------|--------|-------|
| Session 1 — Lease, Registry, ServiceDiscovery, Events, Txn, Space | Done | Merged 2026-03-30. RPC renames, duration negotiation, ServiceID-style re-registration, Contents RPC, handback, and lease-expiry cascades across all four dependent services |
| Session 2 — Space transaction isolation (Space as 2PC participant) | Done | Merged 2026-04-10. `TxnEnlister` trait seam (mirrors `Leasing`/`CapabilityResolver`), auto-enlist on transactional write/take, `LocalTxnEnlister` (bundled) + lazy `RemoteTxnEnlister` (split), end-to-end split_space_txn integration tests |
| Nested transactions (`NestableServerTransaction`) | **Deferred** | Explicitly out of scope for Phase 2 |
| Transaction journal durability / crash recovery | **Gap** | In-memory only, not spec'd for a Phase 2 session yet |
| Typed Entry classes (vs flat string attrs) | **Deliberate divergence** | Flat map is the intentional modernization |

---

## Djinn Split Mode

Each service can boot as its own process, discoverable through Registry. Monolith boot is still the default for local dev. See `.claude/plans/djinn-split/PRD.md`.

| Item | Status | Notes |
|------|--------|-------|
| `coordin8-bootstrap` crate (discover/self-register/watch-expiry) | Done | Shared plumbing for split subcommands |
| `Leasing` trait + `RemoteLeasing` (cross-process lease seam) | Done | `retry_forever` + transport-failure reconnect, pinnable |
| `CapabilityResolver` trait + `RemoteCapabilityResolver` | Done | Registry template resolution seam, same retry pattern |
| Multi-call subcommands: `djinn lease/registry/event/space/txn/proxy` | Done | `djinn` with no subcommand still boots bundled mode |
| Phase 1 — LeaseMgr + Registry split | Done | Self-registration via short-TTL self-lease |
| Phase 2 — EventMgr split | Done | |
| Phase 3 — Space split | Done | |
| Phase 4 — TransactionMgr split | Done | |
| Phase 5 — Proxy split | Done | |
| Chaos tests: RemoteLeasing + Space survive LeaseMgr kill | Done | `coordin8-djinn/tests/split_chaos.rs`, 3-phase pattern |
| Docker-compose chaos (kill a container) | **Gap** | Follow-up from djinn-split PR |
| DynamoDB/MiniStack provider-swap test | **Gap** | Prove the seam across a real provider boundary |
| Registry redundancy in split mode | **Gap** | Currently single Registry — open question from PRD |

---

## Dashboard & Observability

| Item | Status | Notes |
|------|--------|-------|
| Structured logging (tracing crate) | Done | Env-controlled log levels |
| Dashboard UI (React) | Not started | Phase 9 |
| Provenance trace explorer | Not started | |
| Metrics (Prometheus/OTel) | Not started | |

---

## Higher-Order Patterns

Not in core — built on Space/EventMgr primitives. **Unblocked** — Space v1 and EventMgr are both done.

| Pattern | Status | Description |
|---------|--------|-------------|
| Lens | Not started | Reactive derived computation (watch → compute → out) |
| Reflex | Not started | Action executor (take → side effect → out) |
| Sentry | Not started | Constraint watcher (watch → predicate → out) |
| Ledger | Not started | Append-only audit space |
| SignalMesh | Not started | External data ingest connectors |
| GutLog | Not started | Human-in-the-loop intuition logging |
| Information | Not started | Smart proxy with space-indexed stream access |

---

## Suggested Priority Order

1. **Space CLI** — `spaces read/out/take/watch` commands in the Go CLI (Go Space SDK now done)
2. **SDK parity gaps** — Java LeaseClient missing `keepAlive` + `watch`; Space/EventMgr/TxnMgr hand-written clients missing in Java + Node
4. **Djinn split follow-ups** — docker-compose chaos, DynamoDB/MiniStack provider-swap test, Registry redundancy
5. **AWS Provider** — DynamoDB/SQS/EventBridge for production
6. **Higher-order patterns** — Lens, Reflex, Sentry (unblocked by Space + EventMgr)
7. **Dashboard** — observability UI
