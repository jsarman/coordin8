# Session 2 — round-3 re-review (2026-11-14)

A re-review of `main` at `c25f8d6` (all of #70–#75 merged) confirmed N1–N4 fixed and found one new bug introduced by #71's Space reconcile.

**Found:** `TransactionService.GetState` mapped *every* error to `NOT_FOUND`, including a store failure. Space's `reconcile_txns` reads `NOT_FOUND` as "no record, abort". In split mode, one failed txn-store read made Space abort a live transaction: the buffered write was dropped, the later `commit()` returned `Ok`, and the write was lost. Bundled mode was unaffected (`LocalTxnEnlister` already told the cases apart).

**Shipped (one PR, `fix/getstate-not-found`):**
- `GetState` returns `NOT_FOUND` only for `TransactionNotFound`; other errors go through `map_err` (store error → `INTERNAL`, which `RemoteTxnEnlister` treats as "no answer, retry").
- Defence in depth: `reconcile_txns` aborts on "no record" only after two consecutive passes (`unknown_txns` marks, cleared by any real answer or when the Space no longer holds the txn) — same rule as the orphan sweep.
- Tests: `coordin8-txn/tests/get_state.rs::store_error_is_not_reported_as_not_found`; `coordin8-space/tests/reconcile.rs` updated for two passes plus `no_record_must_repeat_consecutively`.

**Noted, no change:** wire vote value 3 meant ABORTED between #65 and #72 and means PREPARED now. Only matters for a participant built outside the repo from `main` in that window; pre-1.0, no external users.

## Round-3 lows (second PR, `fix/round3-lows`)

The round-3 re-check confirmed #76 with a split-mode repro, and listed three lows plus one docs nit:

- **Reconcile stall:** `reconcile_txns` now looks up 8 transactions at a time (`RECONCILE_CONCURRENCY`), and the first failed lookup ends the pass, so a down TxnMgr costs one timeout per pass instead of one per held transaction.
- **Enlist DNS:** participant calls go through a `Dialer` that resolves once, drops metadata-range addresses not explicitly allowed, and connects to exactly the vetted address (same approach as the Proxy's `vet_upstream`). A refused dial never reached the participant, so for a single participant it aborts like an unreachable one.
- **Abort error code:** once the `Aborting` CAS wins, `commit()`/`abort()`/`abort_expired` answer "aborted" even if delivery or the final `Aborted` write fails; recovery finishes it (`deliver_abort_or_defer`).
- **Docs nit:** a pass with no answer does not reset the "no record" mark; CLAUDE.md now says "two 'no record' answers with no other answer between them" instead of "two passes in a row".

Tests: `recovery.rs::{abort_answer_survives_failed_final_write, refused_dial_aborts_single_participant}`, `reconcile.rs::coordinator_down_ends_the_pass_early` — all three fail on the old code.
