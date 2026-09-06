# Registry-Only Bootstrap — PRD

> **Status: Approved, starting implementation 2026-09-06.** Found while trying to run the auction-house demo against `docker-compose.split.yml` (`.claude/plans/hardening-roadmap/`, item 2) — auction-service/settlement-engine/auction-board all take a single `DJINN_HOST` and dial fixed ports on it, which only works for monolith topology. Investigated and rejected two smaller fixes (a Proxy-forwarding sidecar; per-service host overrides in each SDK) before landing on this as the actually-correct one — see Non-Goals for why.

## Goal

A client should need to know exactly **one** address — Registry's — to reach every core Djinn service. It looks up LeaseMgr, Space, EventMgr, Proxy (and TxnMgr, for anything that needs it directly) through Registry, the same way it already looks up application services like Greeter via `ServiceDiscovery`. This makes client code identical whether talking to the bundled monolith or a fully split, multi-host deployment — no per-topology configuration, no per-service address list to maintain.

## Motivation

Jini's own bootstrap needed a single well-known thing: port 4160 multicast discovery (`CAFEBABE` magic packets) to find a Lookup Service, and nothing else — never a list of ports for every service in the system. Coordin8 deliberately dropped multicast (decoupled from the JVM/RMI, per `coordin8-design-napkin.md`), but in doing so, the SDKs ended up needing a *worse* amount of prior knowledge than Jini did: a single host plus five fixed, hardcoded ports (`host:9001` through `host:9006`), which only makes sense if every service happens to live on that one host. Registry is Coordin8's actual analog to the Jini Lookup Service — it should be the *only* thing a client needs to know in advance, exactly like Jini's discovery-then-lookup model, just without multicast.

The gap was invisible until split mode existed. In bundled/monolith mode, "one host, fixed ports" and "the truth" happen to coincide, so nobody noticed the SDKs were bypassing Registry for the core services. Split mode broke that coincidence.

## Discovered Prerequisites (both block correct split-mode operation)

### 1. Bundled mode didn't self-register anything — FIXED (Phase 1, below)

`run_all()` (`djinn/crates/coordin8-djinn/src/services.rs`) — bundled mode — **did not self-register any of its own services into Registry.** No `self_register()` calls anywhere in it, unlike every split-mode function which already does this. So `Registry.Lookup({interface: "LeaseMgr"})` returned nothing when running the monolith. Fixed: bundled mode now self-registers LeaseMgr/EventMgr/Proxy/TransactionMgr/Space the same way split mode does, via a loopback `self_register()` call. Verified live.

### 2. Split-mode Registry's own leases are disconnected from the real LeaseMgr — NOT YET FIXED, found during live testing

`run_registry_on_listener` (split mode's standalone `djinn registry`) builds its own **private, local `LeaseManager`** (own store, own reaper) purely to track its own registrations' TTLs — completely separate from the actual standalone LeaseMgr service (the `lease` container, port 9001) that any client is expected to renew leases against.

**Consequence:** every `lease_id` Registry hands back from `Register()` is only meaningful to Registry's own private tracker. A real client following the documented pattern (`Registry().Register()` → background `Leases().KeepAlive(leaseID, ttl)`) will always get `NotFound: lease not found` from the real LeaseMgr on the very first renewal attempt, because that LeaseMgr never issued the lease in the first place. Confirmed live: registered a Greeter via a container on the split network, watched the entry vanish at the 30s TTL mark, then confirmed directly (`coordin8 lease renew --id <id>`) that the real LeaseMgr had never heard of that lease ID.

**Why this was invisible until now:** no SDK client could previously address split mode's Registry and LeaseMgr correctly at the same time — the old `Connect(host)` assumed one host for every service, which never worked against split mode's multiple hostnames at all. This session's Registry-only bootstrap (Phase 2) was the first thing to actually complete a real Register-then-renew cycle against a running split-mode stack, which is what surfaced it.

**Why the fix is bigger than it looks:** the correct fix is for split-mode Registry to use `RemoteLeasing` (self-referential — Registry can resolve its own `interface: LeaseMgr` lookup once the real LeaseMgr has self-registered) instead of a private local `LeaseManager`, exactly mirroring how EventMgr/Space/TxnMgr already depend on the real LeaseMgr. But that means Registry gains a genuine (if indirect, via itself) dependency on the standalone LeaseMgr container having already self-registered — which means Registry needs the *same* serve-immediately + `PendingLeasing` + health `NotServing`→`Serving` treatment that hardening-roadmap item 1 already gave EventMgr/Space/TxnMgr/Proxy, or boot-order independence (the whole point of that item) regresses for Registry specifically. Not a quick patch — comparable in shape to redoing a slice of item 1, scoped to Registry.

## Plan

### Phase 1 — Rust: self-register bundled mode's services — ✅ DONE

In `run_all()`, after each of LeaseMgr/EventMgr/Proxy/TransactionMgr/Space starts serving, self-register it into the same process's Registry — reusing the existing `self_register()` helper from `coordin8-bootstrap` (split mode already uses this; bundled mode just needs to call it against its own loopback Registry, e.g. `http://localhost:9002`, with a short retry loop for the brief window before Registry's server is actually accepting). No new self-registration mechanism needed — just applying the one that already exists. Verified live: all five self-register immediately; `Registry.Lookup()` now works for every core service in bundled mode. `cargo test --all`/`fmt`/`clippy` all clean.

### Phase 1b — Rust: fix split-mode Registry's disconnected leases — NOT STARTED (see Prerequisite 2 above)

Give split-mode Registry a `PendingLeasing` (self-referential — looks itself up once the real LeaseMgr self-registers) instead of its private local `LeaseManager`, plus the same serve-immediately/health-flip treatment items 1's other services already have. Blocks Phase 5 (split-mode example validation) for anything that registers-then-renews, which is the normal pattern (`hello-coordin8`, `auction-house`'s settlement/auction services all do this).

### Phase 2 — Go SDK — ✅ DONE (Connect() itself; blocked end-to-end by Phase 1b)

`sdks/go/coordin8/client.go`: changed `Connect(host string, ...)` to `Connect(registryAddr string, ...)`. Dials Registry directly at `registryAddr`; looks up LeaseMgr/Proxy/Space/EventMgr via `RegistryServiceClient.Lookup({interface: "X"})` and dials whatever address comes back. Dropped `WithRegistryAddr` (Registry's address is now the primary parameter, nothing to override) but kept `WithLeaseAddr`/`WithProxyAddr`/`WithSpaceAddr`/`WithEventAddr` as explicit escape hatches — if given, skip the lookup for that one service. No dual code path, no deprecated overload — old `Connect(host)` semantics removed outright (SDKs are pre-1.0, no other users yet, per explicit direction).

Also fixed a real bug found in the same pass: `ServiceDiscovery.Get()` (`discovery.go`) hardcoded `"localhost:%d"` for a Proxy-forwarded port, which only worked when Proxy and the consumer shared a network namespace. Now derives the host from the actual address Proxy was dialed at (`sd.client.proxyConn.Target()`), so it's correct whether Proxy is a container, a different host, or genuinely `localhost`.

Updated call sites: `cli/cmd/coordin8/main.go` (`--host` flag renamed to `--registry`), `examples/hello-coordin8/go/greeter_service` and `greeter_client` (`DJINN_HOST` → `COORDIN8_REGISTRY`, matching the naming convention split-mode Rust services already use), `examples/auction-house/settlement-engine` (same rename). `market-watch` and `double-entry` are unaffected — they bypass the shared SDK `Client` entirely and dial one raw gRPC service directly.

Live-verified against split mode inside the actual Docker network (cross-compiled Linux binaries run as throwaway containers on `coordin8_coordin8-split-net`, matching how a real deployed consumer would run) up through Register() and the initial Registry lookup working correctly — surfaced Prerequisite 2 (above) at the lease-renewal step, which is Phase 1b, not a Phase 2 bug.

### Phase 3 — Java SDK

`sdks/java/.../DjinnClient.java`: same shape. Replace (or add alongside, then deprecate) the `connect(host, leasePort, registryPort, proxyPort, spacePort, eventPort)` overload with one that takes only a Registry address and resolves the rest via lookup.

### Phase 4 — Node SDK

`sdks/node/src/djinn-client.ts`: same shape. `DjinnClient.connect(host)` currently hardcodes all 5 ports with zero override — needs the full lookup-based rebuild, not just an added option.

### Phase 5 — Update every example, re-validate against both topologies

- hello-coordin8 (Go service + Go/Java/Node clients)
- market-watch
- double-entry (uses TxnMgr directly, outside the shared `Client` struct — check whether it needs the same treatment or a parallel lookup)
- auction-house (auction-service/Java, settlement-engine/Go, auction-board/Node) — the demo that started this

Each needs re-running against **both** the bundled `docker-compose.yml` and the split `docker-compose.split.yml` — a regression in either direction (break bundled while fixing split, or vice versa) means the phase isn't done.

## Non-Goals / Rejected Alternatives

- **A Proxy-forwarding sidecar** (static 6-port TCP relay in front of the split containers) — rejected. Traced through `ServiceDiscovery.Get()` (Go SDK, `discovery.go`) and found the SDK's own watch-and-refresh caching already provides failover; Proxy in a same-network setup adds a hop with no benefit the SDK doesn't already provide itself. Real value of Proxy is bridging a consumer to a network segment it can't otherwise route to — not exercised by any current example.
- **Per-service host overrides added to each SDK's `Connect()`** (mirroring what Go already partially has) — rejected as a *lesser* fix. It would have unblocked today's demo but still requires an operator/example to know N addresses up front, which is exactly the pattern Jini avoided and this PRD replaces.
- Multicast-style zero-config discovery (true Jini parity) — explicitly out of scope; the design napkin already decoupled from multicast on purpose. Registry-as-single-known-address is the intended replacement, not a step toward multicast.

## Open Questions

1. TxnMgr isn't part of Go's shared `Client` struct today (only Lease/Registry/Proxy/Space/Event) — does it need to join this pattern, or does 2PC enlistment/participant callback wiring stay as its own thing? (`double-entry` constructs its TxnMgr connection separately — check during Phase 5 whether that's incidental or intentional.)

## Decisions

**Backward compatibility: clean breaking change, no deprecation shims.** `Connect(host)`'s signature/semantics change outright (bare host + fixed ports → Registry address + lookup); every call site gets updated directly in Phase 5 rather than carrying an old overload alongside the new one. The SDKs are pre-1.0 and this session's approach throughout has been correctness over compatibility-shimming.
