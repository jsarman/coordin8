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
| Proto: Register, Lookup, LookupAll, Watch | Done | `proto/coordin8/registry.proto` |
| Rust: RegistryIndex + matcher | Done | `coordin8-registry` crate |
| Template ops: exact, contains:, starts_with:, Any | Done | |
| Template ops: Gte, Lte, Range | **Gap** | Napkin mentions them, not implemented |
| TransportDescriptor in Capability | Done | type + config map |
| Go SDK: register, lookup, lookupAll, watch | Done | Full parity |
| Java SDK: lookup, lookupAll | Done | |
| Java SDK: register | **Gap** | Go has it, Java doesn't |
| Java SDK: watch (change events) | **Gap** | Go has it, Java doesn't |
| Node SDK: register, lookup, lookupAll, watch | Done | |
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
| Java: watch-based refresh | **Gap** | Cache never refreshes after initial populate |
| Node: watch, get, close | Done | Cache works |
| Node: watch-based refresh | **Gap** | Cache never refreshes after initial populate |

---

## Space (Tuple Store)

Distributed reactive coordination store. `out/take/read/watch`.

| Item | Status | Notes |
|------|--------|-------|
| Proto: SpaceService definition | Not started | Napkin lines 606-612 |
| Rust: Space crate | Not started | Reuse template matcher from Registry |
| out(tuple, txn, ttl) | Not started | Write a fact into the space |
| take(template, txn, wait) | Not started | Atomic claim + remove |
| read(template, txn, wait) | Not started | Non-destructive read |
| watch(template, on) | Not started | Reactive push, appearance + expiry |
| TTL / lease on tuples | Not started | |
| Provenance metadata | Not started | Who, when, from what input |
| Go SDK | Not started | |
| Java SDK | Not started | |
| Node SDK | Not started | |
| CLI: spaces list, read, out, watch | Not started | |

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
| InMemorySpaceStore | Not started | Needed for Space |
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
| docker-compose.yml | Done | Djinn + greeter, ports 9001–9005 + proxy range |
| host.docker.internal | Done | `extra_hosts: host-gateway` on djinn — 2PC callback routing |
| .dockerignore | Done | |
| LocalStack service | Stubbed | Commented out in compose, ready to activate |
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
| spaces list/read/out/watch | Not started | Blocked on Space |
| trace \<tuple-id\> | Not started | Blocked on provenance |

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

Not in core — built on Space/EventMgr primitives. Blocked until those exist.

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

1. **SDK parity gaps** — Java/Node missing keepAlive, watch, registry refresh; EventMgr + TxnMgr SDK clients
2. **Space** — unlocks the core coordination model (out/take/read/watch)
3. **AWS Provider** — DynamoDB/SQS/EventBridge for production
4. **Dashboard** — observability UI
5. **Higher-order patterns** — Lens, Reflex, Sentry (built on Space + EventMgr)
