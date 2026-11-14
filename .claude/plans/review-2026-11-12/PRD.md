# Code Review Fixes (2026-11-12) — PRD

> **Status: COMPLETE (merged to main, 2026-11-14)** — PRs #63–#69. See `session-1-complete.md`. Follow-up review: [`review-2026-11-14`](../review-2026-11-14/PRD.md). Not started (own initiatives / decisions): H4 (TLS), M1 (scans), M9 (authorization), L5–L7.

## Goal

Fix what the 2026-11-12 review of `main` at `80a3788` found: transactions that can hang forever, Dynamo correctness bugs MiniStack can't show, the Proxy as an unauthenticated network pivot, and delivery contracts (durable events, Notify watches) that don't hold under a dropped connection.

## Motivation

Three issues open since July (coordinator recovery, Dynamo scans, TLS) were still open, and recovery was worse than "v2" implied: a transaction could get stuck in `Voting` with no crash at all. The Dynamo provider never set `consistent_read` and had no `TransactWriteItems`, both invisible on strongly consistent MiniStack. Any caller could register `169.254.169.254:80` and get a tunnel through the Djinn.

## Findings → PRs

| ID | Finding | Sev | PR |
|----|---------|-----|----|
| H1 | Txn stuck in `Voting`/`Prepared` forever | High | #66 |
| H2 | Dynamo: transactional take can lose the tuple | High | #64 |
| H3 | Dynamo: eventually consistent reads (wrong 2PC vote, hung take) | High | #64 |
| H4 | No TLS | High | not started |
| H5 | Proxy forwards anywhere; compose publishes on all interfaces | High | #63 |
| M1 | Full-table scans on hot paths | Med | not started |
| M2 | Durable events acked before the client has them | Med | #67 |
| M3 | Notify watch lease never enforced or renewable | Med | #67 |
| M4 | Blocked `Take` holds the drain, exit 1 | Med | #65 |
| M5 | `VOTE_PREPARED = 0` | Med | #65 |
| M6 | Txn records never deleted | Med | #64 |
| M7 | Forged lease aborts a txn / closes a proxy | Med | #63 |
| M8 | Go SDK: swallowed stream errors, `Get` race, dead watch | Med | #67 |
| M9 | Authentication only, no authorization | Med | not started (decision needed) |
| L1 | Duplicate enlist | Low | #65 |
| L2 | Leases granted before validation | Low | #68 |
| L3 | Proxy: timeouts, error codes / cap, tunnels survive Release | Low | #63 (part) |
| L4 | Bad `MAX_LEASE_TTL` silently 3600; preferred not capped | Low | #68 |
| L5 | Write racing its own commit | Low | not started |
| L6 | Registry Lookup order / expired-unreaped entries / Watch on un-match | Low | not started |
| L7 | `RenewTuple` during a transactional take | Low | not started |
| L8 | Magic numbers for enums | Low | #65 |
| L9 | CI clippy `--all-targets`; CFN PITR/deletion protection; mailbox cap | Low | #68 (mailbox cap not started) |

## Non-goals (this round)

- **TLS (H4).** Needs a design (rustls in tonic, cert distribution, SDK credentials in three languages). Until then: loopback publishing (#63), JWT on, private network or a TLS-terminating tunnel.
- **Scans (M1).** Lease-table GSI keyed on a shard with `expires_epoch`, in-memory Registry/subscription caches, a Space attribute index. Its own initiative.
- **Authorization (M9).** Needs a decision first: per-identity scopes vs. "any valid token is trusted" as an explicit stance; and whether services keep a shared HS256 key.

## Merge order

The PRs branch from `main` independently. #63, #65 and #66 all touch `coordin8-txn/src/manager.rs` (allowlist import, vote decoding, commit/recovery) in different regions; expect small rebase conflicts. #65 and #67 are wire breaks: the Go/Node stubs are regenerated in each, so regenerate again after merging both.
