# Session 1 — complete (2026-11-14)

The follow-up review (`review.md`, of `main` at `b46aefa`) confirmed every 2026-11-12 fix and found four new issues plus nine lows. All but L-a's operator command and L-f (folds into the scan work) are fixed, one PR per theme off `main`:

- **#70 N4** — Dynamo `commit_txn`/`abort_txn` move each buffered row in one `TransactWriteItems` (conditional delete + optional put; shared `move_row` helper). The store API now returns `TxnApplied { published, released, error }`; Space broadcasts / releases leases for whatever moved even on partial failure, then returns the error so the coordinator re-sends.
- **#71 N1 (+L-i, L-a, L-b)** — new internal txn state `Aborting` (decided abort, delivery pending; mirror of `Prepared`, no TTL, reported as ABORTED on the wire). Every abort path records it and only moves to `Aborted` once all participants acked; recovery re-sends. Participant pull: `TxnEnlister::state()`, `SpaceStore::list_txns()`, `SpaceManager::reconcile_txns` every 30s in both boot modes. Recovery: only aborts if its CAS won; per-txn backoff (cap 10 min); 8-way parallel sweep; `coordin8_txn_unresolved` gauge; counts only what it moved.
- **#72 N2** — `PrepareVote`: 0 unspecified, 1 reserved, 2 ABORTED, 3 PREPARED, 4 NOTCHANGED. Every value any older stub sends now decodes as abort. Wire break; stubs regenerated.
- **#73 N3, L-g** — Go `Receive`: taking event N acks only what came before it; the last event taken is never auto-acked. `Receive` response header `coordin8-delivery`; Go sends no acks on best-effort streams; server best-effort Ack skips the mailbox.
- **#74 L-c, L-d, L-e, L-h** — `is_metadata_range` adds `fd00:ec2::/32`; Enlist refuses metadata-range endpoints by default; in-memory finished-txn FIFO; CLAUDE.md CI note.

**Verified:** each branch — `cargo test --all`, clippy `--all-targets`; Dynamo suite on MiniStack (54/54 combined); Go SDK `-race`; Node 53/53, Java build, CLI and Go examples on #72. All five trial-merged in order on `main`: clean, all suites pass.

**Found along the way:** #71 initially conflicted with #70 in Space's `manager.rs` (imports only); rebased #71 onto #70.

**Left:** L-a operator command; everything carried over from review-2026-11-12 (TLS, scans incl. L-f, authorization, L5–L7, proxy connection cap, mailbox cap).
