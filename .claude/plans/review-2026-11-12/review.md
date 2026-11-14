# Coordin8 code review

**Reviewed:** `jsarman/coordin8` `main` at `80a3788` (PR #62, 2026-10-08). No branch on GitHub is ahead of `main`, so anything unpushed on your machine is not covered.
**Date:** 2026-11-12

## Verdict

The in-memory path is in good shape: the 2PC state machine is CAS-guarded, the reaper no longer races renewals, lagged watchers are told, and shutdown drains streams. The gaps are in three places:

1. **The three issues that were open in July are still open.** Coordinator crash recovery, DynamoDB scans, and TLS are all listed as non-goals in `review-2026-10-03`. Recovery is worse than "v2" suggests, because a transaction can get stuck without any crash (H1).
2. **The DynamoDB provider has correctness bugs that MiniStack cannot show you** (H2, H3). They only appear on real DynamoDB or on a crash.
3. **The Proxy is an unauthenticated network pivot by default** (H5).

For a live VouchMe demo, H5 and H4 matter the moment the Djinn is on a network with other devices. H1 to H3 matter if the demo runs on real DynamoDB or anything restarts mid-demo.

## What I did

| | |
|---|---|
| Read line by line | All of `djinn/crates` except `coordin8-observability`; `providers/local`; the core paths of `providers/dynamo`; the Go SDK |
| Skimmed only | Java and Node SDKs, compose files, CFN template, CI workflow |
| Not reviewed | `examples/`, the Go CLI, `coordin8-observability`, split-mode boot code beyond Registry (it mirrors bundled mode) |
| Built and ran | `cargo test --all`: 143 passed, 0 failed, 54 ignored. `cargo clippy --all-targets`: clean |
| Could not run | The 54 ignored tests are the Dynamo provider suite. No MiniStack here, so every Dynamo finding is from reading the code |
| Toolchain caveat | I ran on Rust 1.97.0, not the pinned 1.94.1 |

Each finding is marked **Ran** (I reproduced it with a throwaway test, since deleted; the repo is unchanged) or **Read** (from the code only).

## Findings at a glance

| ID | Finding | Severity | Evidence | Already tracked? |
|---|---|---|---|---|
| H1 | A transaction can be stuck in `Voting`/`Prepared` forever | High | Ran | Partly (as "crash recovery") |
| H2 | Dynamo: a transactional take can lose the tuple | High | Read | No |
| H3 | Dynamo: eventually consistent reads cause missed wake-ups and a wrong 2PC vote | High | Read | No |
| H4 | No TLS anywhere; tokens and data in cleartext | High | Read | Yes (non-goal) |
| H5 | Proxy forwards to any address a caller registers | High | Read | No |
| M1 | Full-table scans on every hot path | Medium | Read | Partly |
| M2 | Durable events are acked before the client has them | Medium | Ran | No |
| M3 | A `Notify` watch lease is never enforced or renewable | Medium | Ran | No |
| M4 | One blocked `Take` makes shutdown wait out the grace period and exit 1 | Medium | Ran | No |
| M5 | `VOTE_PREPARED = 0`: an empty response counts as a yes vote | Medium | Read | No |
| M6 | Transaction records are never deleted | Medium | Read | No |
| M7 | A forged lease can abort someone else's transaction or close their proxy | Medium | Read | No |
| M8 | Go SDK: stream errors swallowed; `ServiceDiscovery.Get` race | Medium | Read | No |
| M9 | Authentication only, no authorization | Medium | Read | Partly |
| L1–L9 | Smaller items | Low | Mixed | Mostly no |

---

## High

### H1. A transaction can be stuck in `Voting` or `Prepared` forever

`coordin8-txn/src/manager.rs:212-222`, `:285-296`

There are three ways in, and no way out:

- **Single participant, any transport error.** `PrepareAndCommit` fails, `commit()` returns `TransactionOutcomeUnknown`, and the state stays `Voting`.
- **Coordinator dies** between the `Voting` CAS and the final CAS.
- **Phase 2 commit call fails.** It is logged ("participant needs recovery") and the transaction is marked `Committed` anyway. The participant never flushes.

Once in `Voting`, `commit()` and `abort()` both return `CommitInProgress`, and `abort_expired` is a no-op. The orphan sweep only lists `Active` transactions.

**Ran:** I enlisted `127.0.0.1:1` (connection refused, so the participant provably never saw the request) and called `commit()`. The result was `OutcomeUnknown`, then `CommitInProgress` on retry, `CommitInProgress` on abort, and a final state of `Voting`.

On the Space side this leaves uncommitted writes and taken tuples sitting in the buffers. When their own leases lapse they are dropped, so a taken tuple is lost rather than restored.

**Fix.** Your state machine already records the decision, so recovery needs no new state:

- `Prepared` means the decision was commit. Re-drive `Commit` to every participant until each acks. Do this on boot and on a timer.
- `Voting` with two or more participants means no decision. Abort all.
- `Voting` with one participant is the ambiguous case. Treat a connect failure as a safe abort, since nothing was sent. For anything else, add a participant status RPC or retry `PrepareAndCommit`, which is already idempotent in Space.
- Let a client `commit()` on a `Prepared` transaction re-drive phase 2 instead of returning `CommitInProgress`.

### H2. Dynamo: a transactional take can lose the tuple

`providers/dynamo/src/space_store.rs:563-599` (and `:536-553` for own writes)

A take under a transaction deletes the tuple from the committed table, then puts it in `txn_taken` as a second, separate call. If the process dies between the two, or the put fails, the tuple is in neither table. It is not restored on abort and no sweep will find it. The client sees an error for a take that destroyed data.

**Fix.** Do both in one `TransactWriteItems` (conditional `Delete` plus `Put`). There is no `TransactWriteItems` call anywhere in the provider today. `commit_txn` and `abort_txn` are also item-by-item, but they are idempotent, so they become safe once H1 gives you something that retries them.

### H3. Dynamo: eventually consistent reads cause missed wake-ups and a wrong vote

No call in `providers/dynamo/src` sets `consistent_read`. MiniStack is strongly consistent, so the tests cannot catch this. Three places read immediately after a write:

- **Wrong 2PC vote.** `has_txn` (`space_store.rs:754`) queries the uncommitted table right after the client's write. A stale read returns "no state", Space votes `NOTCHANGED`, the coordinator records `Committed`, and the writes are never flushed. They vanish when their leases expire. The caller is told the commit succeeded.
- **Hung blocking take.** A write broadcasts in-process, the blocked taker scans within microseconds, misses the new row, and goes back to sleep. Nothing wakes it again until another matching write arrives.
- **Delayed durable event.** `peek` (`event_store.rs:435`) has the same shape: wake-up, stale read, back to waiting.

**Fix.** Set `consistent_read(true)` on base-table `GetItem`, `Query` and `Scan` in these paths. GSI reads cannot be strongly consistent, so `remove_by_lease` will keep relying on the orphan sweep.

### H4. No TLS anywhere

No crate depends on a TLS stack. Every internal dial is `http://`, and the Go SDK hardcodes `insecure.NewCredentials()` (`discovery.go:91`). With JWT auth on, the bearer token crosses the wire in cleartext on every call, so anyone on the path can replay it.

This is fine inside an mTLS mesh such as Istio. It is not fine for three devices on shared Wi-Fi. Until it lands, run the demo on a private network or through a TLS-terminating tunnel.

### H5. The Proxy forwards to any address a caller registers

`coordin8-proxy/src/manager.rs:248-279`, `coordin8-registry/src/service.rs:205-225`

`Register` accepts any `host`/`port` in the transport config. `OpenProxy` then opens a listener that forwards raw TCP there. So any caller can register `169.254.169.254:80` or an internal database and get a tunnel to it through the Djinn. You closed the same hole for transaction participants with `COORDIN8_TXN_PARTICIPANT_ALLOW`; the Proxy has no equivalent.

Two things make it worse:

- Auth is off by default, and the compose files publish 9002–9006 and 9100–9200 on all host interfaces (`docker-compose.yml:34-39`).
- The forwarded ports are never authenticated, even with JWT on. Anyone who can reach an open proxy port reaches the upstream.

**Fix.** Add an upstream allowlist that mirrors the participant one, and deny link-local addresses by default. Publish compose ports as `127.0.0.1:9002:9002`.

---

## Medium

### M1. Full-table scans on every hot path

Your plan tracks `take_match`/`find_match`. The full list is longer:

| Path | Cost |
|---|---|
| Reaper, `lease_store.rs:294` | Filtered `Scan` of each of four lease tables, **every second** (`services.rs:495`) |
| Space `read`, `take`, `contents` | `Scan` of the whole tuple table, payloads included, loaded into memory |
| Blocked takers | Each one re-scans on every matching write |
| Registry `Lookup`, `store.rs:43` | `Scan` per lookup, and the Proxy looks up once per forwarded TCP connection |
| `emit`, `event/manager.rs:140` | `Scan` of all subscriptions per event |
| Orphan sweep, `sweep.rs:77` | `Scan` plus one `GetItem` per resource, every 30s per service |
| Lease cancel after a take | Two extra `Scan`s of the txn buffer tables (`space_store.rs:84`) |

The tables are on-demand billing, so the reaper alone costs read units proportional to table size, 86,400 times a day per table.

**Fix, in order of payoff.** Give the lease tables a GSI keyed on a small shard number with `expires_epoch` as sort key, and `Query` it. Cache the Registry and subscription lists in memory, since each Djinn is the only writer. For Space, either require one indexed attribute per template or keep an in-memory attribute index with Dynamo as the durable copy.

### M2. Durable events are acked before the client has them

`coordin8-event/src/service.rs:178-196`

The loop acks an event once the channel has room again, which means tonic pulled the frame. That is not the same as the client receiving it. Frames sitting in HTTP/2 or socket buffers when the connection drops are already deleted from the mailbox.

**Ran:** three events queued; I pulled two off the handler stream and dropped it, as a dying connection would. The mailbox then held one. Both pulled events were gone without any client seeing them.

This is the residue of finding #5 from October. The delivery contract says at-least-once; this is at-most-once for whatever is in flight.

**Fix.** Only the client can ack. Add an `Ack(registration_id, up_to_seq)` RPC, or have `Receive` carry a resume cursor and delete everything at or below it.

### M3. A `Notify` watch lease is never enforced or renewable

`coordin8-space/src/service.rs:182-257`

`Notify` grants a lease and stores a watch record, then discards the lease id (`let (_, _lease_id)`). `SpaceEvent` has no lease field, so the client cannot renew. And nothing connects the watch record to the stream.

**Ran:** `Notify` with `ttl_seconds: 1`. After 2.5s the watch record was gone, and a tuple written afterwards was still delivered on the stream.

So `ttl_seconds` does nothing, and the Go SDK comment that the channel closes when "the watch lease expires" is wrong. Separately, the forwarding task only notices a disconnected client when a matching tuple arrives. For a template that never matches, the task leaks for the life of the process.

**Fix.** Return the lease in response metadata or a first event, end the stream when the watch is reclaimed, and `select!` on `tx.closed()` the way `best_effort_loop` already does.

### M4. One blocked `Take` makes shutdown wait out the grace period and exit 1

The shutdown signal ends streams. A unary `Take` or `Read` with `wait: true` is not a stream, so it holds the drain.

**Ran:** bundled Djinn with a 3s grace and one client blocked in `Take`. SIGTERM led to exit code 1 after 3.4s, and the client saw `UNKNOWN` rather than `UNAVAILABLE`. At the default grace that is a 20s, non-zero exit on every rolling restart with a waiting worker, which is the normal state of a tuple-space consumer.

**Fix.** Pass the `ShutdownSignal` into `SpaceManager::read`/`take` and return `UNAVAILABLE` when it fires.

### M5. `VOTE_PREPARED = 0`

`proto/coordin8/transaction.proto:105-109`, `coordin8-txn/src/manager.rs:465`

`vote_from_i32` maps unknown values to `Aborted` and its comment says it never defaults toward commit. But proto3's default is 0, and 0 is `PREPARED`. A participant that returns an empty `PrepareResponse` has voted yes.

**Fix.** Renumber with `VOTE_UNSPECIFIED = 0`. It is a wire break, so it is cheapest now, before anyone outside the repo implements `ParticipantService`.

### M6. Transaction records are never deleted

`TxnStore::remove` has no callers, and the Dynamo rows carry no `ttl` attribute. Every finished transaction stays forever. The txn orphan sweep scans the whole table every 30s and filters for `Active` in memory, so its cost grows without bound.

**Fix.** Set a `ttl` when a transaction reaches `Committed` or `Aborted` (a day is plenty for idempotent retries), and enable TTL on `coordin8_txn` in the CFN template.

### M7. A forged lease can abort someone else's transaction

`coordin8-djinn/src/services.rs:826` and `:1554`; `coordin8-proxy/src/manager.rs:197`

`LeaseService.Grant` is public on every service port and accepts any `resource_id`. The txn and proxy cascades act on the `resource_id` of a reclaimed lease, not on whether that lease is the one the resource holds. So: grant a lease for `txn:<id>` on the TxnMgr port, cancel it, and the cascade calls `abort_expired(<id>)` on the real transaction. `proxy:<id>` closes someone's listener the same way.

It needs the victim's id, which is a UUID, so this is not remotely exploitable by guessing. But transaction ids are handed to every participant and logged. The Registry, Space and Event cascades are not affected because they look up by `lease_id`.

**Fix.** Compare `record.lease_id` with the resource's stored lease before acting, or reject `Grant` for the reserved prefixes.

### M8. Go SDK

- **Stream errors are swallowed.** `Receive`, `Notify`, `Watch` and `WatchExpiry` all close the channel on any `Recv` error (`event.go`, `space.go`, `registry.go:153`, `lease.go:198`). The server's `DATA_LOSS` and `UNAVAILABLE` signals, added in October, never reach a Go caller. A closed channel looks the same for lag, shutdown and a clean end. The Java and Node SDKs do surface errors.
- **`ServiceDiscovery.Get` race** (`discovery.go:102`). Two goroutines that miss the cache for the same template both open a proxy. The second one closes the connection the first has already returned to its caller. A server that resolves lazily under concurrent requests will hit this on first use.
- **Dead watch.** `consumeWatch` now discards every event, so each template holds a Registry `Watch` stream and a goroutine for nothing.

### M9. Authentication only

The interceptor validates the token and drops the claims. No handler sees `sub` or `scope`, so any valid token can commit any transaction, take any tuple, or re-point any proxy. And because the secret is a shared HS256 key that every service uses to self-mint, any one compromised service can mint a token for any identity. Worth writing down as an explicit decision if it is one.

---

## Low

- **L1. Duplicate enlist.** `add_participant` does not dedupe. **Ran:** two concurrent first writes under one transaction produced two `Enlist` calls and two participant rows. The commit still works, but it loses the single-participant fast path and prepares the same endpoint twice.
- **L2. Leases granted before validation.** `Space.write` grants the tuple lease before checking the transaction is still open. `begin` and `Register` grant before the store write. Each failure leaves a lease with no resource until its TTL.
- **L3. Proxy hardening.** No connect or idle timeout and no connection cap (listed as Low in October, still open). Every error maps to `NOT_FOUND`, including port exhaustion. Tunnels already open survive `Release`.
- **L4. Lease config.** An unparsable `MAX_LEASE_TTL` silently becomes 3600. `preferred_ttl` is not capped by `max_ttl`.
- **L5. Write racing its own commit.** A transactional write that passes the `enlisted` check just before `commit_space_txn` runs lands in the buffer after the flush. The client gets success and the tuple is dropped when its lease expires.
- **L6. Registry semantics.** `Lookup` returns the first match in map order, so several providers of one interface get no load spreading. It also returns entries whose lease has expired but not yet been reaped. `Watch` filters `MODIFIED` on the new attributes, so a watcher is never told that an entry stopped matching its template.
- **L7. `RenewTuple` during a transactional take.** It looks up by tuple id in the committed table, so the writer's keep-alive gets `NotFound` while another transaction holds the tuple. If that transaction aborts, the restored tuple has lost its renewer.
- **L8. Magic numbers.** Event types `0`/`1`/`2` and votes `0`/`1` are literals rather than the generated enums (`registry/service.rs:183`, `space/participant.rs`).
- **L9. CI and infra.** Clippy is clean with `--all-targets`, so that flag can go into CI now. The CFN tables have no point-in-time recovery or deletion protection. A durable mailbox grows without limit while its lease keeps being renewed.

## What held up

These are worth naming because I looked for problems and did not find them:

- **2PC transitions.** Every state change is compare-and-set. Lease expiry during a commit is ignored, as it should be.
- **Reaper against renewal.** `remove_if_unchanged` closes the race in both providers.
- **FOREVER leases.** I checked that `MAX_UTC` survives the RFC 3339 round trip. It does.
- **Lag handling.** Server streams end with `DATA_LOSS` instead of dropping silently.
- **Orphan sweep.** The two-strike rule is the right call given eventually consistent lease reads.
- **Participant allowlist.** It is checked before anything is stored.

## Suggested order

1. **Before any networked demo:** bind compose ports to loopback, add the Proxy upstream allowlist (H5), turn JWT on, and keep traffic on a private network until TLS exists (H4).
2. **Small and high value:** `consistent_read` (H3), `TransactWriteItems` for the take (H2), shutdown for blocked takes (M4), the vote enum (M5), txn TTL (M6), lease-id check in the cascades (M7). Each is a contained change.
3. **The one real project:** transaction recovery (H1). It also makes the non-atomic Dynamo commit and abort safe.
4. **Then:** client-acked events (M2), the lease-table GSI and in-memory caches (M1), the Go SDK fixes (M8).
