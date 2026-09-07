# coordin8-auth

Shared JWT authentication for every Coordin8 gRPC service. Full design and decisions in [`.claude/plans/grpc-security/PRD.md`](../../../.claude/plans/grpc-security/PRD.md) — this README covers what a deployer actually needs to know to turn it on safely.

## Opt-in, off by default

Set `COORDIN8_JWT_SECRET` and a service validates every incoming RPC's `authorization: Bearer <token>` header (HS256, the same shared secret every service holds). Leave it unset and the service behaves exactly as it always has — plaintext, unauthenticated. Mint tokens offline with `coordin8 auth mint-token --sub <identity>` (see the CLI); there is no runtime issuance service.

## `COORDIN8_AUTH_VERIFY_SIGNATURE=false` — read this before setting it

By default, a configured service cryptographically verifies every token's signature. Setting this to `false` skips that check and only decodes+validates claims (`exp`, `iss` if configured) — a real, supported mode, **not** a footgun left in by accident: it exists for deployments where something upstream already verified the signature (an Envoy JWT filter, an Istio `RequestAuthentication`, an OIDC-terminating ALB, or another Coordin8 service forwarding a token it already checked).

**The precondition this depends on is real, and Coordin8's own topology cuts against it by default:** there is no gateway in Coordin8's architecture — Registry, EventMgr, Space, Proxy, and TransactionMgr are independent peer processes, each its own port, each binding `0.0.0.0`. Lease renewal in particular dials `grantor_host:grantor_port` taken directly off the lease itself, which by construction does not traverse anything. If you set `verify_signature=false`, you are asserting that *every* path that can reach this service's port — including that peer-to-peer one — is already covered by a verifier. If that's not true for your deployment, any client that can reach the port can forge a token for any identity.

A service configured this way logs a `warn!` at startup naming this exact precondition — if you see it and didn't mean to enable this mode, check `COORDIN8_AUTH_VERIFY_SIGNATURE` in that service's environment.

## Other v1 limitations, stated explicitly

- **One shared secret, no audience scoping.** Every service holding `COORDIN8_JWT_SECRET` can mint a token for any `sub`/`scope`. `sub`-based authorization (via `with_claims_check`) is only as strong as the least-trusted service holding the secret.
- **No mTLS, no transport encryption.** JWT auth protects integrity (a token can't be forged without the secret); without TLS, the token itself is visible on the wire and a captured token is replayable until it expires.
- **Authentication only, no RBAC.** A valid token from any configured `sub` can call any RPC.

See the PRD's Non-Goals section for why each of these is deferred rather than fixed here.
