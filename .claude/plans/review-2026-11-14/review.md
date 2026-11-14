# Coordin8 follow-up review

**Reviewed:** `jsarman/coordin8` `main` at `b46aefa` (PR #69, 2026-11-14), covering PRs #63–#68 since `80a3788`.
**Previous review:** `coordin8-review-2026-11-12.md` (your copy is `.claude/plans/review-2026-11-12/review.md`).

## Verdict

The fixes hold. Every finding you marked done is done, and the four I had reproduced by running no longer reproduce. Recovery is the right design: the recorded state is the decision, and the CAS guards make it safe even if two coordinators ever overlap.

The round left one real gap and three smaller ones:

1. **Abort is still best-effort.** Recovery re-drives Commit but never Abort. A participant that misses its Abort is never told, and a tuple taken under that transaction is lost instead of restored (N1).
2. **The vote renumbering is unsafe against old stubs.** A participant built before #65 that votes ABORTED is now read as NOTCHANGED, and the transaction commits (N2).
3. **The Go SDK acks a durable event while the caller is still processing it** (N3).
4. **Replaying a Dynamo commit can resurrect a tuple that was already taken** (N4).

## What I ran

| | |
|---|---|
| Rust | `cargo test --all`: 163 passed, 0 failed, 57 ignored. `cargo clippy --all --all-targets -- -D warnings`: clean |
| Go SDK | `go test -race ./...`: pass. Run in a scratch copy with dependencies pulled from their GitHub mirrors, because the Go module proxy is not reachable from here |
| Not run | The 57 ignored tests (Dynamo, need MiniStack). Java and Node SDK tests. So every Dynamo conclusion is still from reading |
| Toolchain | Rust 1.97.0, not the pinned 1.94.1 |
| Probes | Throwaway tests, deleted afterwards. The repo is unchanged |

**Ran** means I reproduced it. **Read** means from the code only.

## Status of the 2026-11-12 findings

| ID | Finding | Status | How checked |
|---|---|---|---|
| H1 | Txn stuck in `Voting`/`Prepared` | **Fixed** for commit; abort half open, see N1 | Ran: my original repro now ends `Aborted` |
| H2 | Dynamo transactional take loses the tuple | **Fixed** (`TransactWriteItems`) | Read |
| H3 | Eventually consistent reads | **Fixed** on base-table reads | Read |
| H4 | No TLS | Open (your non-goal) | |
| H5 | Proxy forwards anywhere | **Fixed**, with two residuals (L-d, L-e) | Read, plus your tests pass |
| M1 | Full-table scans | Open (your non-goal). Now costs double, see L-f | |
| M2 | Events acked before the client has them | **Fixed** on the server. Go client reintroduces part of it, see N3 | Ran: mailbox keeps all 3 after a dropped stream |
| M3 | `Notify` lease not enforced | **Fixed** | Ran: `REGISTERED` carries the lease; stream ends `NOT_FOUND` when it lapses; watch record removed |
| M4 | Blocked `Take` holds shutdown | **Fixed** | Ran: exit 0 in 0.1s, client sees `UNAVAILABLE` |
| M5 | `VOTE_PREPARED = 0` | **Fixed**, but see N2 | Read |
| M6 | Txn records never deleted | **Fixed** (one-day TTL) | Read |
| M7 | Forged lease aborts a txn or closes a proxy | **Fixed** | Read, plus your test passes |
| M8 | Go SDK stream errors, `Get` race, dead watch | **Fixed** | Go tests pass |
| M9 | No authorization | Open (decision pending) | |
| L1, L2, L4, L8 | Dup enlist, orphan leases, lease config, magic numbers | **Fixed** | Read, tests pass |
| L3 | Proxy hardening | Partly: timeout and status codes done. Connection cap and tunnels surviving `Release` open | Read |
| L5, L6, L7 | Write racing commit, Registry semantics, `RenewTuple` | Open | |
| L9 | CI and infra | Clippy and table protection done. Mailbox cap open | Read |

---

## New findings

### N1. Abort is never re-driven, and a taken tuple is lost (High in split mode)

`coordin8-txn/src/manager.rs`: `do_abort_participants` logs a failed Abort call and moves on. `recover()` only looks at `Voting` and `Prepared`.

Every abort path has this, and #66 added one where it is guaranteed: a single participant that cannot be reached is aborted on the spot, so by construction that participant is never told.

**Ran, coordinator side:** two participants, B down at commit. The transaction aborts and A gets its Abort. I then brought B up and ran `recover()` three times. It resolved nothing, and B received zero Abort calls.

**Ran, with Space:** a tuple with a 3s lease, taken under a transaction, with Space's participant port down at commit.

- The commit returns `transaction aborted`, which is right.
- After Space's port is back and `recover()` has run, Space still holds the transaction's state and the tuple is not visible.
- When the tuple's lease lapses it is removed. It is never restored.

The caller was told the transaction aborted, which promises the take was undone. It was not.

In bundled mode this is hard to hit, because Space is in the same process. In split mode on Dynamo it only takes a Space restart during a commit or a lease-expiry abort: the buffers survive the restart, and nobody comes back for them.

**Fix.** Two options, and they work well together:

- **Coordinator pushes.** Treat abort like commit: a state that means "decided abort, delivery pending" (the mirror of `Prepared`), re-driven by `recover()` until every participant acks. Only then set the TTL.
- **Space pulls.** In Space's sweep, for each transaction it holds buffers for, ask TxnMgr `GetState`. `Aborted` or not found means abort locally; `Committed` means commit locally. This also heals a participant the coordinator can no longer reach. It is only safe because `Prepared` records never expire, so that invariant needs to stay.

### N2. Old-stub ABORTED now reads as NOTCHANGED (Medium, if any participant lives outside this repo)

`proto/coordin8/transaction.proto`

The renumbering shifted every value up by one:

| Wire value | Before #65 | Now |
|---|---|---|
| 0 | PREPARED | UNSPECIFIED, treated as abort |
| 1 | NOTCHANGED | PREPARED |
| 2 | ABORTED | NOTCHANGED |
| 3 | | ABORTED |

**Ran:** two participants, one voting PREPARED and one sending wire value 2, which is what an old stub sends for a veto. `commit()` returned `Ok`, the state was `Committed`, and the yes-voter received Commit.

Everything in the repo was regenerated, so nothing here is affected. The risk is any `ParticipantService` built against the old stubs elsewhere, during the window before it is rebuilt.

**Fix.** Renumber so every old value fails safe: `reserved 1;`, keep `VOTE_ABORTED = 2`, and use `VOTE_PREPARED = 3`, `VOTE_NOTCHANGED = 4`. Old 0 and 1 then decode as abort and old 2 is still abort. It is one more wire break, and it is cheapest today.

### N3. The Go SDK acks an event the caller is still processing (Medium)

`sdks/go/coordin8/event.go`, `stream.go`

`Receive` marks an event as taken the moment the caller reads it from the channel, and a 250ms ticker acks whatever is taken. So the event in the caller's hands is acked while it is being handled. The final flush on stream end does the same.

**Ran against a live Djinn:** two durable events. I took event 1, held it for 600ms as if processing, then cancelled. The next `Receive` started at seq 2. Event 1 was gone.

Java acks after `onEvent` returns and Node acks when the loop asks for the next event. Both are correct, so Go is the outlier, and `CLAUDE.md` describes all three as acking after processing.

**Fix.** When event N is taken, the caller has finished N−1, so ack N−1. Do not ack the last taken event when the stream ends. The cost is one redelivered event after an idle reconnect. If that matters, add an explicit `Ack` or `Done` for callers who want precise control.

### N4. Replaying a Dynamo commit or abort can resurrect a taken tuple (Medium, Read)

`providers/dynamo/src/space_store.rs`, `commit_txn` and `abort_txn` (unchanged this round)

Each tuple is moved with two calls: put into the tuple table, then delete from the buffer. If the delete fails or the process dies between them, the tuple is visible and still in the buffer. Recovery now replays the commit, which is correct, and the replay puts the tuple again. If a worker took it in between, it comes back and is processed twice.

Before #66 nothing replayed these calls. Now replays are routine, and a throttled `DeleteItem` is enough to trigger one. In my earlier review I said recovery would make these paths safe. That was only true for loss, not for duplication.

A second effect of the same partial failure: tuples flushed before the error are never broadcast, so a blocked taker is not woken for them.

**Fix.** Move each tuple in one `TransactWriteItems`, with the delete conditional on the buffer row existing. `move_to_txn_taken` is already the pattern.

### Low

- **L-i. Recovery sends Abort even when its CAS lost (Read).** In `recover_one`, the multi-participant `Voting` branch calls `finish` and then `do_abort_participants` unconditionally. If `finish` returns `None`, someone else moved the state, possibly to `Prepared`, and the participants are told to abort a transaction whose decision is commit. One process cannot hit this, because the in-flight claim excludes it. Two coordinators can: during a split-mode rolling restart the new TxnMgr runs `recover()` at boot while the old one is still finishing commits. The window is the gap between a read and a CAS, so it is narrow. The fix is one line: only send Abort when `finish` returned `Some`.
- **L-a. Recovery never gives up and has no signal.** A `Prepared` transaction whose participant is gone for good is retried every 30s forever, with a warning each time. The sweep is sequential with a 5s connect and 10s call timeout, so a batch stuck on one dead endpoint delays everything behind it. Add a metric for stuck transactions, backoff, and an operator command to force an outcome.
- **L-b. `recover()` over-counts.** A transaction that finishes between the list and the claim is returned as resolved and logged as "transaction recovered".
- **L-c. In-memory txn store.** `prune_finished` walks every finished transaction on each `create`, so `begin` is linear in a day's worth of transactions.
- **L-d. IPv6 metadata address.** The link-local default covers `169.254/16` and `fe80::/10`. AWS's IPv6 metadata endpoint, where it is enabled, is `fd00:ec2::254`, which is neither. Add `fd00:ec2::/32` to the default refusal.
- **L-e. Enlist has no link-local default.** The Proxy refuses link-local unless named. `Enlist` with no allowlist still dials anything.
- **L-f. Scans cost double now.** `consistent_read` is right, and it doubles the read units of every full-table scan in M1. That moves the lease-table GSI and the in-memory caches up the list.
- **L-g. Go ack traffic on best-effort streams.** `Receive` sends an `Ack` every 250ms of activity for registrations where it is a no-op. Each one is a subscription read plus a mailbox query.
- **L-h. Doc drift.** `CLAUDE.md` still says CI omits `--all-targets` in two places, and the review PRD status still reads "PRs open".

## What held up

- **Recovery and live commits.** The in-flight claim is taken before the CAS, and recovery re-reads under the claim. I looked for a way to have recovery act on a commit in progress and did not find one.
- **Two coordinators, mostly.** The in-flight set is per process, so I walked through two TxnMgrs on one table. The CAS makes the cases converge on one outcome, with the single exception in L-i. The usual cost of an overlap is that the new coordinator aborts multi-participant commits the old one had in flight, and their callers get an error.
- **Proxy vetting.** The address that is checked is the address that is dialed, so a DNS answer cannot change in between. IPv4-mapped IPv6 is canonicalized before the link-local test.
- **Notify teardown.** Lease reclaim, client disconnect, lag and shutdown all end the stream and release the watch.

## Suggested order

1. **N1.** It is the other half of H1, and it loses data.
2. **N2**, if anything outside this repo implements `ParticipantService`. Do it before those are rebuilt against the new numbering.
3. **N3, N4 and L-i.** All three are small. N3 and N4 are about the at-least-once and take-once guarantees, and L-i is a one-line guard.
4. Then the items already on your list: TLS, the scan work (now more pressing), and the authorization decision.
