# Code Review Fixes (2026-10-03) — PRD

> **Status: In progress.** Phase 1 PRs open (#41–#45); Phase 2: I (#48), J (#46, #47) open; F/G/H unblocked by D1–D4 (decided 2026-10-03). Enlist SSRF (finding 12a) still needs a design call.

## Goal

Fix the correctness, durability, and security issues found in a full review of `main` at `6ed23d5`, close the test/CI gaps that let them through, and refresh the project's Claude tooling (agents, skills, `CLAUDE.md`) and docs, which had drifted far enough to *cause* at least one of the bugs.

## Motivation

A whole-repo review found bugs that every existing test misses, because they live in code paths CI never runs: the Dynamo provider (all 36 of its tests are `#[ignore]`d), timing races (lease expiry during 2PC commit), and restarts. Several break the project's core promises: a 2PC commit can report success while participants roll back; durable events can be lost; FOREVER leases are reaped immediately on Dynamo.

One root cause is tooling drift: `.claude/skills/lease-provider/SKILL.md` still documents the pre-swap sentinels (`LEASE_FOREVER = 0`) and tells implementers to exclude FOREVER leases with `ttl_seconds == 0` — exactly the stale filter in `DynamoLeaseStore::list_expired`.

## Findings → workstreams

Severity from the review. "WS" = workstream (one branch/PR each).

| # | Finding | Sev | WS |
|---|---------|-----|----|
| 1 | 2PC: unconditional state writes + live txn lease → lease expiry mid-commit aborts participants while coordinator writes `Committed`; concurrent `commit()`s race; late `Enlist` is skipped | High | C |
| 2 | Dynamo: FOREVER leases reaped every sweep (`MAX_UTC` RFC3339 string sorts before now; `ttl_seconds <> 0` filter is stale) | High | D |
| 3 | Dynamo: `list_expired` scan not paginated — reaper stops seeing expired leases past the first 1 MB page | High | D |
| 4 | Dynamo: mailbox key `(registration_id, seq_num)` + unconditional put; `seq_num` is per event type and resets on restart → events overwrite each other | High | F (Phase 2) |
| 5 | EventMgr: `Receive` deletes mailbox before delivering; live-delivered durable events never removed → loss on disconnect, replay on reconnect | High | F (Phase 2) |
| 6 | Space: tuple lease expiring while its txn is open → commit/abort publishes a tuple with a nonexistent lease (immortal tuple) | High | E |
| 7 | Reaper deletes a lease renewed between `list_expired` and `remove` | Med | D |
| 8 | Registry: re-`Register`/`ModifyAttrs` accept any `capability_id`; `Lookup` exposes them → entry hijack | Med | G (Phase 2) |
| 9 | Proxy: no lease on `OpenProxy`; crashed clients leak ports (101-port range in compose) | Med | G (Phase 2) |
| 10 | Core services renew by re-`Register` → `MODIFIED` every ~10s → Go `ServiceDiscovery` closes held `ClientConn`s | Med | H (Phase 2) |
| 11 | Client-facing watch streams (Registry `Watch`, `WatchExpiry`, Event `Receive`) silently drop lagged events | Med | H (Phase 2) |
| 12 | TxnMgr dials any caller-enlisted endpoint with a fresh token (SSRF); single-participant timeout marked `Aborted` though outcome unknown | Med | C (outcome) / G (SSRF) |
| 13 | No SIGTERM handling / graceful drain | Med | I (Phase 2) |
| — | `chrono::Duration::seconds` panics on huge TTL when `MAX_LEASE_TTL=forever` | Low | D |
| — | Dynamo `batch_write_item` ignores `UnprocessedItems` | Low | F |
| — | Taken-under-txn tuple leases not cancelled on commit | Low | E |
| — | Proxy: no upstream connect/idle timeout | Low | G |
| — | `Dockerfile.djinn` stale `EXPOSE 9001` + LeaseMgr comment; runs as root | Low | B |
| — | CI: Dynamo tests never run; SDKs/CLI have zero tests; Java/Node not in CI; clippy lacks `--all-targets` | Gap | D (MiniStack job), J |

## Phase 1 — no decisions needed (running now)

| WS | Branch | Scope |
|----|--------|-------|
| A | `chore/claude-tooling-refresh` | `.claude/agents/*`, `.claude/skills/*`, root `CLAUDE.md` facelift. Fix inverted sentinels in `lease-provider`, rewrite stale `provider-wiring`, add frontmatter (name/description) to the six domain skills so they auto-load, remove `/home/jsarman` paths from `test-runner`, drop LeaseMgr from `documenter`. Model aliases (`model: sonnet`) stay — they already track the latest Sonnet; pinning IDs would re-stale them. |
| B | `docs/drift-sweep-2026-10` | User-facing docs only (READMEs, `Dockerfile.djinn` EXPOSE/comment, `docs/`), verified against code. Not `CLAUDE.md` (owned by A). |
| C | `fix/txn-2pc-state-races` | Finding 1 + single-participant unknown-outcome half of 12. Compare-and-set state transitions on `TxnStore` (both providers), stop the txn lease from aborting a commit already past `Active`, reject `Enlist` once voting starts. |
| D | `fix/lease-store-correctness` | Findings 2, 3, 7, Duration panic; MiniStack CI job so Dynamo tests actually run. |
| E | `fix/space-txn-lease-lifecycle` | Finding 6 + taken-tuple lease cleanup. |

## Phase 2 — needs decisions (see below)

| WS | Scope | Blocked on |
|----|-------|-----------|
| F | Event durability (4, 5, `UnprocessedItems`) | D3 |
| G | Ownership/abuse: registry hijack (8), proxy leases (9), enlist SSRF (12) | D1, D2 |
| H | Watch lag signalling (11) + `MODIFIED` churn (10) | D4 |
| I | Graceful shutdown (13) | — (sequenced after C to avoid conflicts in `services.rs`) |
| J | SDK + CLI test suites; Java/Node in CI; `clippy --all-targets` | — (sequenced last; large) |

## Decisions — DECIDED 2026-10-03 (John: "go with your recommendations")

- ✅ **D1 — Registry ownership.** Decided: re-`Register`/`ModifyAttrs` must present the entry's `lease_id`, which acts as a capability token (it's already returned only to the registrant and never exposed by `Lookup`). Works with auth off. Alternative: bind entries to the JWT `sub` (only works with auth on).
- ✅ **D2 — Proxy leases.** Decided: `OpenProxy` grants a lease and Proxy becomes a Landlord like the other services (mounts `LeaseService`); SDK `ServiceDiscovery` keeps it alive. Proto change + all three SDKs. Alternative: idle-timeout reclaim only (no proto change, weaker).
- ✅ **D3 — Event delivery semantics.** Decided: keep at-least-once; assign a per-*registration* monotonic sequence at enqueue (atomic counter on the subscription item in Dynamo), delete a mailbox entry only after it's sent on the stream, and dedupe by that per-registration sequence.
- ✅ **D4 — Watch lag.** Decided: on lag, send a terminal `DATA_LOSS`/`ABORTED` status so the client resubscribes and re-snapshots (SDKs already reconnect), rather than silently continuing. And only emit `MODIFIED` on re-`Register` when interface/attrs/transport actually changed.

## Non-goals

- TLS / mTLS (tracked separately under grpc-security).
- 2PC coordinator crash recovery (sweeping `Voting`/`Prepared` on restart) — v2; the CAS work in C is a prerequisite.
- DynamoDB `take_match` scan-per-call performance.

## Process

Each workstream: Sonnet subagent in its own worktree, commits locally, does **not** push. The coordinator session verifies the diff and tests, then pushes and opens the PR. PRs merge to `main` independently; no stacking.
