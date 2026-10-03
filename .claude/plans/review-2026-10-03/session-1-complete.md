# Session 1 Complete — Code Review Fixes (2026-10-03)

One session took a full-repo review of `main` at `6ed23d5` to fully merged fixes: 14 PRs (#40–#53), all merged the same day. Every finding in the PRD's table is fixed except what's listed under "Not done".

## How it ran

- The coordinator session did the review, wrote the PRD (#40), and split the work into workstreams with disjoint file ownership.
- Each workstream ran as a Sonnet subagent in its own git worktree. The agent committed locally but never pushed.
- The coordinator then re-read every diff, re-ran the gates itself, and did its own red/green checks where the agent's were weak, before pushing and opening the PR.
- John stayed remote (Remote Control) and answered decisions D1–D4 and the enlist-SSRF option asynchronously.
- Conflicts between PRs were resolved by merging `main` into the branch (never rebase + force-push), and every PR was re-verified on top of the new `main` before merge.

## What shipped

| PR | Fix |
|----|-----|
| #40 | The plan (this folder) |
| #41 | Docs drift. Dockerfile: real ports, non-root user. `Cargo.lock` committed: the image previously couldn't build from a clean clone. |
| #42 | Agents, skills, and CLAUDE.md refreshed. Root cause: `lease-provider` skill had the lease sentinels inverted, which produced the Dynamo FOREVER bug. |
| #43 | 2PC: compare-and-set state transitions; a lease expiring mid-commit can no longer abort a commit that reported success. |
| #44 | Dynamo lease store: FOREVER leases are no longer reaped; reaper scan paginated; reaper no longer deletes a just-renewed lease; huge-TTL panic fixed. **First CI job that runs the Dynamo tests (MiniStack).** |
| #45 | Space: a tuple whose lease expires mid-txn is no longer published or restored (previously immortal); taken-tuple leases are cancelled on commit. |
| #46, #47 | First Go/CLI/Java/Node test suites (~200 tests); Java + Node added to CI; Node `keepAlive` pre-aborted-signal bug fixed. |
| #48 | Graceful shutdown: deregister immediately, end streams with UNAVAILABLE, drain, `stop_grace_period`. `docker stop` with a client connected: 3.2s hard kill → 0.12s clean. |
| #49 | Registry: re-Register/ModifyAttrs require the entry's `lease_id` (no hijack); MODIFIED only on real change; watch lag → DATA_LOSS. |
| #50 | Proxies are leased (Proxy is a Landlord; 13th DynamoDB table). |
| #51 | Event durability: per-registration seq (no overwrites), peek/ack (no loss on disconnect, no replay), at-least-once. |
| #52 | TransactionMgr opt-in participant allowlist (`COORDIN8_TXN_PARTICIPANT_ALLOW`); warns when auth is on with no allowlist. |
| #53 | SDK ServiceDiscovery: one leased proxy per template; held connections survive service changes. |

## Things worth knowing next time

- **Stale tooling causes bugs.** The inverted sentinels in a skill shipped a data-loss bug. Treat `.claude/` drift as a correctness issue (now encoded in `reviewer.md`).
- **A clean text merge doesn't prove the merge is correct.** Merging `main` (#48) into #51 auto-merged cleanly but silently dropped the shutdown wrapper on #51's new BestEffort `Receive` path. It was caught only by reading the merged code; a regression test now covers both delivery modes.
- **Agent red/green claims need spot checks.** Where an agent's revert was crude, the coordinator re-ran a surgical red check: restoring only the old `list_expired` body made the FOREVER test fail against live MiniStack.
- **Review agent trade-offs too.** The first SDK discovery fix "retained replaced proxies until Close()" — an unbounded client-side leak that would have recreated #50's port exhaustion. Sent back. A regression test now asserts at most one live proxy after repeated expire/register cycles.
- **`Cargo.lock` is tracked now.** Branches that add dependencies must commit their lockfile updates; verify with `cargo check --locked`.
- **Shared MiniStack.** Parallel agents shared one MiniStack on :4566. Tests use UUID table names, so sharing is safe; agents must not start or stop it.

## Not done (still open / non-goals)

- 2PC coordinator crash recovery (sweeping `Voting`/`Prepared` on restart). #43's CAS is the prerequisite. A txn whose single-participant commit outcome is unknown now stays `Voting` (honest), and only recovery can resolve it.
- Dynamo `take_match`/`find_match` scan per call. #45 adds buffer-table scans on the Dynamo cancel path; a GSI on the buffer tables would remove them (CFN change).
- CI clippy still omits `--all-targets` (run it locally).
- Stale generated `sdks/node/gen/**/*.{js,d.ts}` from the original Node SDK commit are never imported; delete them sometime.
- TLS/mTLS (grpc-security non-goal).
