# gRPC Security via JWT — PRD

> **Status: Planning — not started.** This is hardening-roadmap item 6, the one item left in `.claude/plans/hardening-roadmap/PRD.md` (items 1-5 all done). Explicit user direction: plan this properly before writing any code, the same way `.claude/plans/distributed-leasing/` got a full research-then-design pass before implementation — this is bigger and more cross-cutting than anything else on that roadmap.

## Goal

Every gRPC call into a Coordin8 service — Registry, Space, EventMgr, TransactionMgr, Proxy — carries a signed JWT proving the caller's identity, and every service independently validates it before doing any work. Today, all traffic is fully unauthenticated and unencrypted (every SDK dials with `insecure.NewCredentials()` / `usePlaintext()` / `grpc.credentials.createInsecure()`); anyone who can reach a Djinn's ports can register services, write tuples, grant leases, or vote in a 2PC transaction as anyone else.

## Motivation

Coordin8 models itself on Jini/JavaSpaces, and Jini (from 2.0 / Apache River onward) does have a real security model — `net.jini.security`, built on JAAS `Subject`s and `MethodConstraints` (`ClientAuthentication`, `ServerAuthentication`, `Integrity`, `Confidentiality`) that a service can require per-method. That model is deeply Java-specific (JAAS, Java security policy files, RMI's own security-aware invocation layer) and doesn't port to a polyglot, non-JVM system by design — Coordin8 already deliberately decoupled from the JVM (`coordin8-design-napkin.md`). JWT-over-gRPC is the practical, language-agnostic equivalent: every mainstream gRPC implementation (tonic, grpc-go, grpc-java, `@grpc/grpc-js`) has a first-class extension point for exactly this (gRPC's own **call credentials** concept, distinct from **channel credentials**/transport security — the two compose).

It's alpha, pre-1.0, no external users — same reasoning that drove the leasing rewrite and registry-bootstrap: correcting this now costs one cross-cutting change across the Rust core + 3 SDKs; leaving it until there are real deployments/users costs a breaking migration under pressure.

## How gRPC + JWT actually works (grounding, not hand-waving)

gRPC has two independent credential layers that compose:
- **Channel credentials** — transport-level (TLS vs. plaintext).
- **Call credentials** — per-RPC, attached as metadata (typically `authorization: Bearer <token>`). This is where a JWT lives.

Client side, every language has the same shape: attach an interceptor/call-credentials object that inserts the bearer token into outgoing metadata on every call.
- **Go**: `credentials.PerRPCCredentials` (`GetRequestMetadata`), passed via `grpc.WithPerRPCCredentials`.
- **Java**: `io.grpc.CallCredentials` (or a `ClientInterceptor`), applied per-stub or per-channel.
- **Node** (`@grpc/grpc-js`): a `CallCredentials` object (`generateMetadata`), combined with channel credentials via `grpc.credentials.combineCallCredentials`.
- **Rust (tonic)**: `tonic::service::Interceptor`, wrapping the channel, injecting into `Request::metadata_mut()`.

Server side: an interceptor/layer runs before every handler, reads `authorization` from incoming metadata, validates the JWT (signature, expiry, issuer), and rejects with `UNAUTHENTICATED` if missing/invalid. Since every Coordin8 service is Rust/tonic (SDKs are clients only), server-side validation logic lives in exactly one place — a new shared crate — not duplicated per-language.

**Important nuance a naive read of "secure gRPC via JWT" misses:** a JWT's signature protects *integrity* (can't be forged/tampered without the key), not *confidentiality* — without TLS, the token itself is visible in plaintext to anyone on the wire, and a captured token can be replayed until it expires. JWT auth and TLS transport encryption are related but separate concerns; see Non-Goals for how this PRD sequences them.

## Decisions (resolving the hardening-roadmap stub's open questions)

1. **Signing: shared-secret HMAC (HS256) for v1, not asymmetric (RS256/JWKS).** Every validating service holds the same secret (`COORDIN8_JWT_SECRET`), validated locally with no network call. Simpler to operate for an alpha system with a small, mutually-trusted set of services. Trade-off, stated plainly: any one compromised service that leaks the secret can forge tokens for every identity — this is an intentional v1 simplification, not an oversight. **Documented upgrade path, not a promise to build now:** move to RS256 + JWKS (an issuer's private key, every validator fetches/caches the public key) once there's an actual trust boundary between services worth defending — e.g., a real multi-tenant or third-party-integration deployment. Tracked as a non-goal below, not a future phase of this PRD.

2. **Token issuance: static, pre-shared tokens — no live "AuthMgr"/token-issuance service.** This is the load-bearing decision that avoids reinventing the leasing bootstrap-cycle problem: if tokens were minted by a runtime service that a caller had to discover-and-call first, Registry's own self-registration would face the exact "can't discover the thing I need to bootstrap through the thing I'm bootstrapping" cycle solved once already this project ([[coordin8_distributed_leasing]] / registry-bootstrap Phase 1b). Static tokens sidestep this entirely: a service reads its own token from config (`COORDIN8_TOKEN` env var) at startup, the same way it already reads `COORDIN8_REGISTRY` — no RPC needed to obtain it, so there's no cycle to break. A new CLI subcommand (`coordin8 auth mint-token --sub <identity> --ttl <duration>`) generates tokens offline using the shared secret; this is an operator/deploy-time action, not a runtime service.

3. **Every service validates independently — no gateway.** Coordin8's split mode has no single ingress choke point (Registry/Space/EventMgr/TxnMgr/Proxy are independent peer processes, each its own port) — "validate at the gateway" doesn't fit this architecture. A new shared library crate (`coordin8-auth`, alongside existing shared crates like `coordin8-lease`/`coordin8-bootstrap`) provides one `tonic` interceptor every service's server mounts identically, so the logic exists exactly once despite running in five places.

4. **Authentication only for v1, not fine-grained authorization.** The interceptor checks "is this a validly-signed, non-expired token from a trusted issuer" uniformly for every RPC on every service — it does not yet check "is this identity *allowed* to call `Space.Take`." The JWT's claims (`sub`, and an optional `scope`) are carried through and available to a future authorization layer, but v1 doesn't gate on them. Matches the project's existing bias toward the simplest correct thing (see distributed-leasing's own precedent) rather than building RBAC nobody's asked for yet.

5. **mTLS: explicitly not chosen for v1** (see Non-Goals). JWT-over-gRPC is the mechanism this PRD implements; mutual TLS is a heavier, infra-team-owned alternative (cert issuance/rotation, usually delegated to a service mesh like Istio/Linkerd) that doesn't fit a project with no mesh and no CA infrastructure today.

## Plan

### Phase 1 — Rust core: shared `coordin8-auth` crate + wire into every service

- New crate `djinn/crates/coordin8-auth`: JWT encode/decode (HS256, via the `jsonwebtoken` crate), a `Claims` struct (`sub`, `exp`, `iat`, optional `scope`), and a `tonic` `Interceptor` implementation that extracts `authorization: Bearer <token>` from incoming request metadata, validates it against `COORDIN8_JWT_SECRET`, and rejects with `Status::unauthenticated` on anything wrong (missing header, bad signature, expired, malformed).
- Every service's tonic server (Registry, Space, EventMgr, TransactionMgr, Proxy — both bundled `run_all()` and each split-mode `run_*_on_listener`) mounts this interceptor identically, via `coordin8-djinn/src/services.rs`'s existing per-service construction helpers (the same place `embedded_landlord()` already lives, from the distributed-leasing work).
- `djinn` binary reads `COORDIN8_JWT_SECRET` at startup (fails fast if unset, once this ships — no silent unauthenticated fallback).
- New CLI subcommand: `coordin8 auth mint-token --sub <identity> [--ttl <duration>]` — reads the same secret, prints a signed token. This is the only way tokens get created in v1; no runtime issuance endpoint.
- The `coordin8` CLI itself needs a token for every command that talks to a live Djinn — `--token` flag / `COORDIN8_TOKEN` env var, mirroring the existing `--registry` convention.

### Phase 2 — Go SDK

- `coordin8.Connect(registryAddr string, opts ...ConnectOption)` gains `WithToken(token string)` (or reads `COORDIN8_TOKEN` if unset — exact precedence to nail down during implementation, not blocking the plan). Wires a `credentials.PerRPCCredentials` implementation attaching the bearer token to every call across all four channels the `Client` holds (registry/proxy/space/event) plus any ad-hoc `DialLease`/`NewLeaseClient` connections.
- CLI (`cli/cmd/coordin8`) picks up `--token`/env var and passes it through to `Connect`.

### Phase 3 — Java SDK

- `DjinnClient.connect(registryAddr, ...)` gains a token parameter (exact overload shape — options object vs. additional positional arg — decided during implementation, following the same judgment-call pattern Phase 3 of registry-bootstrap already used for pinned-address overloads). Wires `io.grpc.CallCredentials` (or a `ClientInterceptor`) onto every channel.

### Phase 4 — Node SDK

- `DjinnClient.connect(registryAddr, opts?)` — `opts.token` (or `COORDIN8_TOKEN`). Wires a `CallCredentials` object via `grpc.credentials.combineCallCredentials` onto every channel.

### Phase 5 — Update every example, re-validate against both topologies

- Every `docker-compose*.yml` (bundled, split, auction-house) needs `COORDIN8_JWT_SECRET` on every Djinn service and a minted `COORDIN8_TOKEN` on every client (greeter, auction-service, settlement-engine, auction-board) — likely via a small setup script that mints tokens for the compose file's known identities rather than hand-editing UUIDs into YAML.
- Re-run hello-coordin8 (Go/Java/Node), market-watch, double-entry, auction-house against both bundled and split-mode stacks — a regression in either direction isn't done, same bar registry-bootstrap Phase 5 already set.

## Non-Goals (for this PRD — not forever, just not now)

- **TLS transport encryption.** Related to this work (a leaked/sniffed token is replayable without it) but a separate, sequenced concern — this PRD ships JWT authentication over whatever transport already exists (plaintext in dev/Docker-Compose today) and documents the residual risk rather than silently ignoring it. A follow-up PRD can add TLS channel credentials once there's a real deployment target that needs it (cert issuance is itself a whole decision — self-signed for dev, a real CA or ACME for anything else).
- **mTLS / certificate-based service identity.** Heavier than this project's current operational maturity supports (no service mesh, no CA). JWT is the chosen v1 mechanism; mTLS is a plausible *later* upgrade for a specific deployment target (e.g. once on a mesh), not a parallel track.
- **Dynamic token issuance/rotation service.** Static, CLI-minted tokens only for v1 — see Decision 2. Rotation is a manual "mint a new token, redeploy" operation, not automated.
- **Fine-grained authorization / RBAC.** V1 is authentication-only (valid token or not) — see Decision 4.
- **Asymmetric signing (RS256/JWKS).** HS256 shared-secret for v1 — see Decision 1.
- **End-user / multi-tenant application identity.** This secures service-to-service and operator/CLI traffic into the Djinn itself, not whatever application-level user auth a system built on top of Coordin8 might need.

## Open Questions (to resolve during implementation, not blocking this plan)

1. Exact SDK API shape per language for supplying a token (env var only vs. explicit option vs. both, and precedence if both given) — follow each SDK's own established idiom, same judgment-call latitude Phase 3/4 of registry-bootstrap already used successfully.
2. Whether `COORDIN8_JWT_SECRET` being unset should be a hard failure (recommended — no silent unauthenticated mode) or an explicit opt-out for local dev convenience (e.g. `mise r djinn` without ceremony) — leaning hard-failure-by-default with a clearly-named escape hatch (e.g. `COORDIN8_AUTH_DISABLED=true`) rather than making insecure-by-default the path of least resistance.
3. Token TTL defaults and whether long-running services (which don't currently have any "refresh my credential" loop, unlike leases) need one, or whether a generously long default TTL (e.g. 30-90 days) plus manual rotation is acceptable for v1 — leaning toward the latter given static issuance, but worth a real decision once implementation starts.
