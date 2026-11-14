# Follow-up Review Fixes (2026-11-14) — PRD

> **Status: In progress** — PRs #70–#74 open (2026-11-14), plan docs #75. See `session-1-complete.md`. Not started: L-a's operator "force outcome" command, plus everything carried over from [`review-2026-11-12`](../review-2026-11-12/PRD.md) (H4 TLS, M1 scans, M9 authorization, L5–L7, proxy connection cap, mailbox cap).

## Goal

Close what the follow-up review of `main` at `b46aefa` found after #63–#69: abort was still best-effort (the other half of H1), the #65 vote renumbering could commit a vetoed transaction from an old-stub participant, the Go SDK acked an event still being processed, and recovery's routine Commit/Abort replays could resurrect a taken tuple on Dynamo.

## Motivation

Recovery (#66) made the recorded state the decision for commit, but abort was fire-and-forget: a participant that missed its Abort kept the txn's buffers forever, and a tuple taken under an aborted txn was lost — while the caller had been told the take was undone. #66 also made that guaranteed for an unreachable single participant. Recovery replays turned a rare Dynamo two-call move into a routine double-publish risk.

## Findings → PRs

| ID | Finding | Sev | PR |
|----|---------|-----|----|
| N1 | Abort never re-driven; taken tuple lost | High (split) | #71 |
| N2 | Old-stub ABORTED reads as NOTCHANGED | Med | #72 |
| N3 | Go SDK acks an event the caller is still processing | Med | #73 |
| N4 | Dynamo commit/abort replay can resurrect a taken tuple | Med | #70 |
| L-i | Recovery sends Abort after losing its CAS | Low | #71 |
| L-a | Recovery never gives up, no signal | Low | #71 (backoff, parallel sweep, `coordin8_txn_unresolved` gauge); operator command not started |
| L-b | `recover()` over-counts | Low | #71 |
| L-c | In-memory txn prune linear per `begin` | Low | #74 |
| L-d | AWS IPv6 metadata `fd00:ec2::/32` not refused | Low | #74 |
| L-e | Enlist has no metadata-range default | Low | #74 |
| L-f | Consistent reads double scan cost | Low | not started — folds into M1 (scans) |
| L-g | Go ack traffic on best-effort streams | Low | #73 |
| L-h | Doc drift | Low | #74, #75 |

## Non-goals

- TLS, the scan initiative and the authorization decision (carried over, own initiatives).
- An operator command to force a stuck transaction's outcome (L-a remainder).

## Merge order

#70 before #71 (#71 is rebased on #70 — its diff shows #70's commit until #70 merges; Space reconcile can apply Commit/Abort concurrently with the coordinator, which is only safe on Dynamo with #70's atomic moves). #72, #73, #74 are independent. #75 (this plan) last. All five were trial-merged together in that order: clean, Rust suite + clippy, Dynamo 54/54, Go `-race` pass.
