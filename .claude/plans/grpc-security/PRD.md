# gRPC Security via JWT — PRD

> **Status: In progress — all 5 phases implemented, not yet merged to main.** This is hardening-roadmap item 6, the one item left in `.claude/plans/hardening-roadmap/PRD.md` (items 1-5 all done). Implementation lives on branch `worktree-grpc-security-phase1` (worktree `.claude/worktrees/grpc-security-phase1`): Rust core + CLI mint-token (Phase 1), Go/Java/Node SDK token support (Phases 2-4), and an opt-in auth overlay for every example compose stack (Phase 5) are all done and live-verified end-to-end. No PR opened yet — update this line to COMPLETE with the PR link once merged.

## Goal

Every gRPC call into a Coordin8 service — Registry, Space, EventMgr, TransactionMgr, Proxy — can carry a signed JWT proving the caller's identity, which the receiving service validates before doing any work. Today, all traffic is fully unauthenticated and unencrypted (every SDK dials with `insecure.NewCredentials()` / `usePlaintext()` / `grpc.credentials.createInsecure()`); anyone who can reach a Djinn's ports can register services, write tuples, grant leases, or vote in a 2PC transaction as anyone else. This capability is **opt-in configuration**, not a mandatory mode every deployment is forced into on day one — see Decision 6.

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

6. **Auth enforcement is optional config, off unless turned on — resolves what was Open Question 2.** If `COORDIN8_JWT_SECRET` (or however the config ends up named) isn't set, the interceptor doesn't mount at all and the service behaves exactly as it does today — plaintext, unauthenticated. There's no hard-fail-on-unset-secret behavior. Stated plainly, since this is a real trade-off and not just a convenience: a deployment that never configures this gets none of the protection this PRD adds, silently. That's the deliberate choice — matches every other Coordin8 knob so far (`COORDIN8_PROVIDER`, `COORDIN8_ADVERTISE_HOST`, etc. all default to the simplest local-dev behavior and require explicit opt-in for anything more), and avoids forcing every existing example/compose file to be touched just to keep working. Whoever turns this on for a real deployment is making an explicit, visible choice to do so.

7. **The interceptor is configurable, not a fixed black box — covers both "custom claim logic" and "skip signature verification."** `coordin8-auth` exposes an `AuthConfig` a service builds from its own env/config rather than a single hardcoded check:
   - `secret: Option<String>` — `None` means Decision 6's off-by-default case.
   - `verify_signature: bool` (default `true` whenever a secret/config is present) — when `false`, the interceptor decodes the JWT's claims without cryptographically verifying the signature. **Real, deliberate use case, not a footgun left in by accident:** if a service sits behind something that already verified the token — an infra-team-owned gateway or mesh sidecar in front of it, or (within Coordin8's own graph) a service-to-service hop where the immediate caller is itself a `coordin8-auth`-validated Coordin8 service forwarding a token it already checked moments ago — re-verifying the same signature again is pure redundant crypto work for no additional safety. This is an explicit per-service opt-in, not a default, and the doc/comment on the field says exactly what it's trusting: "the network path guarantees only already-validated requests reach this service — verify that's actually true for your topology before setting this."
   - An optional claims hook — a closure/trait (`Box<dyn Fn(&Claims) -> Result<(), Status>>` or similar; exact shape is an implementation detail) a service supplies to layer additional checks on top of the baseline (`exp`, and `iss` if configured) — e.g. requiring a specific `scope` claim, rejecting certain `sub` values, anything a specific service needs that isn't generic enough to bake into the shared crate. Baseline checks still run if no hook is supplied; the hook adds to them, it doesn't require reimplementing them.

8. **Outbound (Djinn-to-Djinn) calls get a token from a pluggable `ClientAuthConfig`, not one hardcoded strategy — the same "configurable, not fixed" philosophy as Decision 7, applied to the client side.** Found while starting implementation: several internal calls exist that Phase 1's original wording didn't cover — `self_register` (every split-mode service registering into Registry), `RemoteCapabilityResolver` (Proxy's remote capability lookups), and `TransactionManager`'s four `ParticipantServiceClient` call sites (2PC prepare/commit/abort/vote against Space). Turning auth on without addressing these would silently break self-registration and 2PC. Rather than pick one fixed answer, `ClientAuthConfig` supports three strategies, chosen per-deployment:
   - **`trust`** (the default) — attach no token to outbound calls at all. A legitimate, explicit choice for a deployment that considers Djinn-to-Djinn traffic already trusted by some other means (network segmentation, a mesh, etc.) — not just "auth not implemented yet."
   - **`self_minted`** — a service mints its own short-lived token for its own outbound calls, using the same shared secret it already validates incoming tokens against. The simplest "just make turning this on actually work end-to-end" option for v1's HS256-shared-secret model (Decision 1).
   - **A custom provider closure** (`from_provider`) — the real reason this needs to be pluggable rather than fixed: in at least one real deployment shape, a service-to-service JWT is synthesized by infrastructure from an mTLS certificate's CN (a sidecar or mesh terminates mTLS, derives an identity, mints or forwards a JWT for it) — Coordin8 itself doesn't generate that token, it just needs an extension point to go get whatever token already exists for this call, wherever that deployment's mechanism puts it. This is explicitly not a case this PRD tries to enumerate or build a specific integration for — the closure is the entire interface.

   The interceptor itself is always the same concrete type regardless of strategy (`ClientAuthInterceptor`, wrapping whichever provider closure — `None` for `trust`) — no dynamic dispatch or per-strategy code paths needed at call sites, only different construction.

## Plan

### Phase 1 — Rust core: shared `coordin8-auth` crate + wire into every service

- New crate `djinn/crates/coordin8-auth`: JWT encode/decode (HS256, via the `jsonwebtoken` crate), a `Claims` struct (`sub`, `exp`, `iat`, optional `scope`), an `AuthConfig` (Decision 7: `secret: Option<String>`, `verify_signature: bool`, optional claims hook), and a `tonic` `Interceptor` built from that config. The interceptor is a no-op (service behaves as today) when `secret` is `None`; otherwise it extracts `authorization: Bearer <token>` from incoming metadata, verifies the signature unless `verify_signature: false`, decodes claims either way, runs the baseline checks (`exp`, `iss` if configured) plus any supplied claims hook, and rejects with `Status::unauthenticated` on anything wrong.
- Every service's tonic server (Registry, Space, EventMgr, TransactionMgr, Proxy — both bundled `run_all()` and each split-mode `run_*_on_listener`) mounts this interceptor identically, via `coordin8-djinn/src/services.rs`'s existing per-service construction helpers (the same place `embedded_landlord()` already lives, from the distributed-leasing work) — applied per-service via generated `with_interceptor()` constructors, not a server-wide layer, so the standard gRPC health check (`tonic_health`) stays exempt (an orchestrator probing liveness shouldn't need a token). `AuthConfig` is built from env vars at startup — `COORDIN8_JWT_SECRET` (unset = disabled, Decision 6), plus whatever env var ends up controlling `verify_signature` (implementation detail, not blocking this plan).
- **Also covers the internal call sites found during implementation (Decision 8):** `self_register`, `RemoteCapabilityResolver`, and `TransactionManager`'s `ParticipantServiceClient` calls each get a `ClientAuthConfig`, defaulting to `trust` (no token, no behavior change) unless a service is configured otherwise.
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
2. Token TTL defaults and whether long-running services (which don't currently have any "refresh my credential" loop, unlike leases) need one, or whether a generously long default TTL (e.g. 30-90 days) plus manual rotation is acceptable for v1 — leaning toward the latter given static issuance, but worth a real decision once implementation starts.
3. Exact env var name and shape for controlling `verify_signature` (Decision 7) — a plain boolean (`COORDIN8_AUTH_VERIFY_SIGNATURE=false`) is probably enough for v1; whether it ever needs to be more granular (e.g. per-caller rather than per-service) is a real question but not one worth answering before there's a concrete deployment that needs it.
4. Exact shape of the claims hook (Decision 7) — a Rust closure only covers the Rust core; if a *future* need arises for an operator (not a Rust-writing developer) to configure claim rules declaratively (e.g. "require scope=admin for Space.Take") without recompiling, that's a bigger feature (a small rule DSL or config format) — explicitly not needed for v1's use cases, noted here so it isn't forgotten if it comes up.
