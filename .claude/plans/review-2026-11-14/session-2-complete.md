# Session 2 — round-3 re-review (2026-11-14)

A re-review of `main` at `c25f8d6` (all of #70–#75 merged) confirmed N1–N4 fixed and found one new bug introduced by #71's Space reconcile.

**Found:** `TransactionService.GetState` mapped *every* error to `NOT_FOUND`, including a store failure. Space's `reconcile_txns` reads `NOT_FOUND` as "no record, abort". In split mode, one failed txn-store read made Space abort a live transaction: the buffered write was dropped, the later `commit()` returned `Ok`, and the write was lost. Bundled mode was unaffected (`LocalTxnEnlister` already told the cases apart).

**Shipped (one PR, `fix/getstate-not-found`):**
- `GetState` returns `NOT_FOUND` only for `TransactionNotFound`; other errors go through `map_err` (store error → `INTERNAL`, which `RemoteTxnEnlister` treats as "no answer, retry").
- Defence in depth: `reconcile_txns` aborts on "no record" only after two consecutive passes (`unknown_txns` marks, cleared by any real answer or when the Space no longer holds the txn) — same rule as the orphan sweep.
- Tests: `coordin8-txn/tests/get_state.rs::store_error_is_not_reported_as_not_found`; `coordin8-space/tests/reconcile.rs` updated for two passes plus `no_record_must_repeat_consecutively`.

**Noted, no change:** wire vote value 3 meant ABORTED between #65 and #72 and means PREPARED now. Only matters for a participant built outside the repo from `main` in that window; pre-1.0, no external users.
