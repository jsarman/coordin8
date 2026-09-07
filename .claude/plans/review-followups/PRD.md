# Review Follow-ups (2026-09-07) — PRD

> **Status: In progress — all 6 findings plus a follow-up review's 4 residuals are implemented, not yet merged to main.** Addresses the actionable findings from an independent code review at commit `145d87a` (`coordin8-review-2026-09-07.md`, supplied by the user). TLS and coordinator recovery (the review's own item 7) are explicitly deferred to their own future topic folder(s) — both are larger, cross-cutting efforts the review itself says don't belong bolted onto this pass. One extra fix beyond the review's own findings, surfaced while chasing down Finding 2: `infra/dynamodb-tables.cfn.yml` still provisioned a single `coordin8_leases` table that the distributed-leasing runtime hasn't looked for since it landed — split into the four namespaced tables the runtime actually requests, live-verified against real DynamoDB (MiniStack). A second review pass of PR #38 itself (same file, updated) found 3 non-blocking residuals plus a minor naming issue — all 4 fixed, see "Follow-up round" below.

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

## Implementation — all done, live-verified, not yet merged

| # | What | Commit |
|---|------|--------|
| 1 | Self-registration recovers from `NotFound`; `self_register_retrying()` closes the initial-registration retry gap too | `07fe670` |
| 2 | Docs corrected (README.md, djinn/README.md, coordin8-djinn/README.md, coordin8-lease/README.md, sdks/go/README.md, auction-house compose) + the CFN/DynamoDB table-naming bug found along the way | `566200d` |
| 3 | `verify_signature=false` startup warning + new `coordin8-auth/README.md` | `fc173c2` |
| 4 | `gen-auth-env.sh` — secret via env not argv, `chmod 600`, TTL/revocation trade-off documented | `1a48d09` |
| 5 | Multi-replica grantor pinning documented as a constraint | `444f135` |
| 6 | `RenewAll` batch cap + `/metrics` 404/405 routing | `5c87f46` |

Every commit was live-verified against real running services (not just unit tests) — see each commit message for the specific verification performed.

## Follow-up round — a second review of PR #38 itself

The same reviewer looked at PR #38's actual diff (not just the original codebase) and confirmed all 6 findings were addressed well, calling out the subprocess-based regression test and the `verify_signature` warning specifically. It found 3 non-blocking residuals plus one minor naming issue — all fixed:

| # | What | Commit |
|---|------|--------|
| 1 | `cli/README.md` still said `9001 lease, 9002 registry` — missed by `566200d`'s doc sweep. Also broader staleness found while fixing it: wrong global flag name, wrong lease flag names, missing `space`/`auth mint-token` docs | `ae02d13` |
| 2 | Agent tooling (`stack-up`/`stack-down` SKILL.md, `test-runner.md`) polled `nc -z localhost 9001` for readiness — a port nothing binds, so every run burned the full 60s (20×3s) and reported the stack down. Switched to 9002 (Registry) | `6ea3c5c` |
| 3 | Self-registration recovery (`07fe670`) only re-registered on `NotFound`; widened to also treat `FailedPrecondition` as "entry unusable, re-register" — the code path `Register`'s own re-registration hits when the entry survives but its lease alone expired (real window with the Dynamo provider, where entry+lease persist independently, unlike local where a restart drops both together). Extracted `entry_is_unusable()`, added a `coordin8-registry` unit test that reproduces the exact `FailedPrecondition` response deterministically (grant a real short lease, let it expire with no reaper running), and a `coordin8-bootstrap` unit test on the predicate itself — verified red against the pre-fix (`NotFound`-only) behavior before restoring the fix | `d8d61f2` |
| 4 (minor) | `SelfRegistrationHandle::capability_id()`/`lease_id()` renamed to `initial_capability_id()`/`initial_lease_id()` — a recovery re-registration changes the live IDs but these accessors didn't track that, so the old names silently went stale after the first recovery. Only consumed by one boot-time `info!` log per service, so a rename to make the contract honest was enough — no `Arc<Mutex<_>>` state for correctness nobody's using yet | `d8d61f2` |

## Non-Goals (for this pass — the review's own item 7, and beyond)

- **TLS anywhere.** Already a known, explicitly deferred non-goal of `grpc-security/PRD.md`; this review just re-raises its urgency (a stolen bearer token is now a 30-day credential under the current minting defaults). Real work, needs its own topic folder.
- **2PC coordinator recovery** (sweeping `Prepared`/`Voting` transactions on restart). Already tracked as a known v2 concern in `coordin8-txn/src/manager.rs`'s own comments; not touched here.
- **DynamoDB `take_match` scan-per-call.** Known, tracked separately (the lease GSI work already shows the pattern to follow); out of scope for this pass.
- **Per-mounted-service `verify_signature`** (review's 3b — "trust the gateway path, verify the peer path" as a single process's mixed policy). Real plumbing, the review itself frames it as "if it's worth it" rather than a clear ask; not done here, left as a documented limitation (3c is addressed — stated as an explicit limitation — but the finer-grained control itself is not built).
- **JWT audience scoping / per-service secrets.** Inherent to the v1 shared-secret HS256 model (`grpc-security/PRD.md` Decision 1's documented trade-off); this pass states it explicitly where it wasn't already, doesn't change the signing model.
