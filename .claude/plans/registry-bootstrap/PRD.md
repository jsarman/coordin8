# Registry-Only Bootstrap — PRD

> **Status: Approved, starting implementation 2026-09-06.** Found while trying to run the auction-house demo against `docker-compose.split.yml` (`.claude/plans/hardening-roadmap/`, item 2) — auction-service/settlement-engine/auction-board all take a single `DJINN_HOST` and dial fixed ports on it, which only works for monolith topology. Investigated and rejected two smaller fixes (a Proxy-forwarding sidecar; per-service host overrides in each SDK) before landing on this as the actually-correct one — see Non-Goals for why.

## Goal

A client should need to know exactly **one** address — Registry's — to reach every core Djinn service. It looks up LeaseMgr, Space, EventMgr, Proxy (and TxnMgr, for anything that needs it directly) through Registry, the same way it already looks up application services like Greeter via `ServiceDiscovery`. This makes client code identical whether talking to the bundled monolith or a fully split, multi-host deployment — no per-topology configuration, no per-service address list to maintain.

## Motivation

Jini's own bootstrap needed a single well-known thing: port 4160 multicast discovery (`CAFEBABE` magic packets) to find a Lookup Service, and nothing else — never a list of ports for every service in the system. Coordin8 deliberately dropped multicast (decoupled from the JVM/RMI, per `coordin8-design-napkin.md`), but in doing so, the SDKs ended up needing a *worse* amount of prior knowledge than Jini did: a single host plus five fixed, hardcoded ports (`host:9001` through `host:9006`), which only makes sense if every service happens to live on that one host. Registry is Coordin8's actual analog to the Jini Lookup Service — it should be the *only* thing a client needs to know in advance, exactly like Jini's discovery-then-lookup model, just without multicast.

The gap was invisible until split mode existed. In bundled/monolith mode, "one host, fixed ports" and "the truth" happen to coincide, so nobody noticed the SDKs were bypassing Registry for the core services. Split mode broke that coincidence.

## Discovered Prerequisite (blocks everything else)

`run_all()` (`djinn/crates/coordin8-djinn/src/services.rs`) — bundled mode — **does not self-register any of its own services into Registry.** No `self_register()` calls anywhere in it, unlike every split-mode function which already does this. So today, `Registry.Lookup({interface: "LeaseMgr"})` returns nothing when running the monolith. Switching the SDKs to a Registry-lookup bootstrap would break bundled mode entirely unless this is fixed first.

## Plan

### Phase 1 — Rust: self-register bundled mode's services

In `run_all()`, after each of LeaseMgr/EventMgr/Proxy/TransactionMgr/Space starts serving, self-register it into the same process's Registry — reusing the existing `self_register()` helper from `coordin8-bootstrap` (split mode already uses this; bundled mode just needs to call it against its own loopback Registry, e.g. `http://localhost:9002`, with a short retry loop for the brief window before Registry's server is actually accepting). No new self-registration mechanism needed — just applying the one that already exists.

### Phase 2 — Go SDK

`sdks/go/coordin8/client.go`: change `Connect(host string, ...)` to `Connect(registryAddr string, ...)`. Dial Registry directly at `registryAddr`; look up LeaseMgr/Proxy/Space/EventMgr via `RegistryServiceClient.Lookup({interface: "X"})` and dial whatever address comes back. Keep the existing `WithLeaseAddr`/`WithRegistryAddr`/etc. `ConnectOption`s as explicit escape hatches — if given, skip the lookup for that one service and use the pinned address instead (useful for tests, or genuinely split-network scenarios where the lookup path isn't reachable).

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
