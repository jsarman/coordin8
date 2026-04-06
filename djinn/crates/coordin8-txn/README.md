# coordin8-txn

TransactionMgr — real two-phase-commit coordinator. Modeled after `net.jini.core.transaction.server`.

## Where It Fits

Layer 4 (port `9004`). Each transaction holds a lease from [`coordin8-lease`](../coordin8-lease/README.md); if that lease expires before commit, the coordinator auto-aborts (per the Jini spec: "let the lease expire and the transaction auto-aborts").

Participants are **not** in-process — they run their own gRPC `ParticipantService` and enlist by endpoint. The coordinator dials each participant during prepare/commit/abort.

## Protocol

1. `Create` — issues a txn id and a lease
2. `Join` — participants enlist with their gRPC endpoint
3. `Commit` —
   - Single participant → `PrepareAndCommit` (saves a round-trip)
   - Multiple participants → parallel `Prepare`, then `Commit` only if all voted PREPARED
   - `NOTCHANGED` voters are skipped at commit
   - Any vote of `ABORT` → coordinator aborts everyone
4. `Abort` — explicit abort or lease expiry → `Abort` sent to all participants

## Layout

```
src/
  lib.rs       re-exports TxnManager + TxnServiceImpl
  manager.rs   TxnManager — state machine, parallel prepare, abort_expired
  service.rs   tonic server impl for TransactionService
tests/         2PC happy-path + veto-abort coverage
```

## Build / Test

```bash
cargo build -p coordin8-txn
cargo test  -p coordin8-txn
```

Live demo against a running Djinn:

```bash
mise r demo-txn    # double-entry — happy commit + veto abort
```

## Notes

- The Djinn binary subscribes to lease expiry and calls `abort_expired` for any `txn:` resource id
- On Linux, the Djinn container needs `extra_hosts: host.docker.internal:host-gateway` so the coordinator can call back to participant servers running on the host
