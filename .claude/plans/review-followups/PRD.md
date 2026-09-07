# Review Follow-ups (2026-09-07) — PRD

> **Status: In progress.** Addresses the actionable findings from an independent code review at commit `145d87a` (`coordin8-review-2026-09-07.md`, supplied by the user), following the review's own suggested order. TLS and coordinator recovery are explicitly deferred to their own future topic folder(s) — both are larger, cross-cutting efforts the review itself says don't belong bolted onto this pass.

## Goal

Fix the one silent/permanent failure mode the review found (self-registration can't recover from an expired lease), correct documentation that now describes a deleted architecture, hedge the JWT auth `verify_signature=false` escape hatch with the warnings its own precondition requires, harden the token-minting script, resolve the multi-replica grantor-address question explicitly, and cap/route two small operational rough edges (`RenewAll`, the metrics endpoint).

## Motivation

An external review of the post-observability-merge codebase (`145d87a`) found real, verified issues — spot-checked against the actual code before starting this work, not taken on faith. The most severe (self-registration recovery) is a genuine silent-permanent-failure bug: a service that misses one lease-renewal window becomes invisible in Registry forever while continuing to run healthy, with nothing but a repeating `warn!` line to notice by. The documentation gap undercuts the project's own best recent work (distributed leasing) by presenting the architecture it replaced. Everything here is scoped from the review's own "Suggested order" section, items 1-6 (item 7 — TLS, then coordinator recovery — is explicitly out of scope for this pass).

## Decisions

1. **Self-registration recovers from `NotFound` by re-registering fresh**, not by treating it as one more transient error. The renewal task's request state becomes mutable: on `NotFound`, it clears `capability_id`, calls `register` again as a fresh registration, and adopts whatever new `capability_id`/lease the response carries for all subsequent renewals. `FailedPrecondition`-class transport errors keep being logged-and-retried unchanged (still transient); only `NotFound` (the entry is confirmed gone) triggers re-registration.

2. **Documentation gets corrected in place, not rewritten wholesale.** Every stale `LeaseMgr`/`:9001` reference the review found gets updated to the actual current architecture (Registry/EventMgr/Space/Proxy/TransactionMgr, distributed leasing, `grantor_host`/`grantor_port`) — this is a correction pass, not a docs redesign.

3. **`verify_signature = false` gets a startup warning, not a behavior change.** The mode itself is legitimate (Decision 7 of `grpc-security/PRD.md` already documents the precondition); what's missing is *telling the operator* their config now depends on every request actually routing through a verifying gateway, in a topology where peer-to-peer dialing (lease renewal via `grantor_host:grantor_port`, binding `0.0.0.0`) means that's not automatically true. A loud `tracing::warn!` at the point `AuthConfig` is built with signature verification off, plus a README line stating the precondition, closes the gap without touching the mechanism itself.

4. **`gen-auth-env.sh` stops passing the secret as an argv.** `coordin8 auth mint-token` already reads `$COORDIN8_JWT_SECRET` — the script already exports it, so dropping the redundant `--secret "$secret"` flag removes the `ps`-visible secret with no behavior change. The generated env file also gets `chmod 600` before anything is written to it. Token TTL/revocation (the review's other note on this script) is a real v1 limitation already implicitly accepted by the JWT PRD's static-token model — not something a shell script can fix — so it gets a comment explaining the trade-off rather than a code change.

5. **Multi-replica grantor pinning: document as a constraint, not a code change.** Stamping a service-level (VIP/DNS) address instead of the granting replica's own address would be a real architecture change (every replica would need to agree on/discover that shared address, and the self-describing-lease design — `grantor_host`/`grantor_port` carried on the `Lease` itself — would need a second, different addressing mode for the shared-store case). That's more than this pass should take on. Documented instead, in the DynamoDB provider's own docs and `distributed-leasing/PRD.md`, as an explicit constraint: single-replica-per-namespace when using a shared backing store, until/unless a real multi-replica deployment need justifies the bigger fix.

6. **Two small operational fixes**: `RenewAll` rejects (rather than silently accepting) a batch above a fixed cap, and the `/metrics` HTTP handler returns 404 for any path other than `/metrics` and 405 for any method other than `GET`, instead of dumping the full metrics body for every request regardless of path/verb.

## Non-Goals (for this pass — the review's own item 7, and beyond)

- **TLS anywhere.** Already a known, explicitly deferred non-goal of `grpc-security/PRD.md`; this review just re-raises its urgency (a stolen bearer token is now a 30-day credential under the current minting defaults). Real work, needs its own topic folder.
- **2PC coordinator recovery** (sweeping `Prepared`/`Voting` transactions on restart). Already tracked as a known v2 concern in `coordin8-txn/src/manager.rs`'s own comments; not touched here.
- **DynamoDB `take_match` scan-per-call.** Known, tracked separately (the lease GSI work already shows the pattern to follow); out of scope for this pass.
- **Per-mounted-service `verify_signature`** (review's 3b — "trust the gateway path, verify the peer path" as a single process's mixed policy). Real plumbing, the review itself frames it as "if it's worth it" rather than a clear ask; not done here, left as a documented limitation (3c is addressed — stated as an explicit limitation — but the finer-grained control itself is not built).
- **JWT audience scoping / per-service secrets.** Inherent to the v1 shared-secret HS256 model (`grpc-security/PRD.md` Decision 1's documented trade-off); this pass states it explicitly where it wasn't already, doesn't change the signing model.
