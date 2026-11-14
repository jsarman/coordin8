# Session 1 (2026-11-13) — complete

Worked the review in its suggested order, one PR per area, all off `main`, each tested locally (Rust full suite + clippy `--all-targets`; Dynamo suite against local MiniStack; Go `-race`, Java, Node where touched).

**Shipped (PRs open):**
- **#63** H5 + M7 (+ L3 part): `EndpointAllowlist` moved to `coordin8-core`, shared by TxnMgr and the new `COORDIN8_PROXY_UPSTREAM_ALLOW`; link-local refused unless named; the proxy dials the vetted resolved address. Compose publishes on `127.0.0.1` (`COORDIN8_PUBLISH_HOST`). Txn/proxy cascades check the reclaimed lease is the resource's own.
- **#64** H2 + H3 + M6: `consistent_read` on base-table reads (reaper scan exempt on purpose); transactional take is one `TransactWriteItems`; finished txns get a one-day `ttl` (shared `FINISHED_TXN_RETENTION_SECS`), in-memory store prunes the same.
- **#65** M4 + M5 + L1 + L8: `VOTE_UNSPECIFIED = 0` (wire break); blocked Take/Read end `UNAVAILABLE` on shutdown (only the idle wait is interruptible); enlist deduped (Dynamo via a string set in the condition); generated enums instead of literals.
- **#66** H1: `TxnManager::recover` (boot + every 30s, plus on a client `commit()` retry). `Prepared` re-drives Commit; single-participant `Voting` re-asks `PrepareAndCommit`; other `Voting` aborts. Phase-2 failure leaves `Prepared`; a never-connected single participant aborts immediately. Participant calls documented as idempotent.
- **#67** M2 + M3 + M8 (wire + Go API break): `Ack` RPC and `ReceiveRequest.after_seq`; server never deletes on send; all three SDKs ack after processing. Notify sends `REGISTERED` with the watch lease first, ends `NOT_FOUND` on reclaim, cancels on client exit; SDKs renew. Go streams are `*Stream[T]` with `Err()`; `ServiceDiscovery.Get` single-flights per template; dead Registry watch removed.
- **#68** L2 + L4 + L9: lease env vars fail fast; preferred capped by max; no orphan leases on failed write/begin/register; CI clippy `--all-targets`; CFN `ProtectTables` parameter.

**Found along the way:**
- H3 can't be reproduced on MiniStack (strongly consistent); fixed from reading only.
- Recovery re-sends Commit to *every* participant of a `Prepared` txn (the record doesn't say who voted NOTCHANGED), which is why Commit for an unknown txn must be a no-op. Space already is.
- Single-participant recovery trusts the participant's retry answer; a participant that aborted and lost the reply would be recorded `Committed` with nothing applied. Harmless for Space (nothing was applied), worth noting for other participants.

**Not done:** H4, M1, M9, L5, L6, L7, L3 remainder (connection cap, tunnels surviving Release), mailbox size cap. See the PRD table.
