# double-entry

Live demo of the Coordin8 [TransactionMgr](../../djinn/crates/coordin8-txn/README.md). Simulates a double-entry bookkeeping transfer between two accounts using real 2PC.

For project context see the [root README](../../README.md).

## What It Does

Two participants — `ledger-debit` and `ledger-credit` — each run their own gRPC `ParticipantService` and enlist in a transaction managed by the Djinn.

| Scenario | Outcome |
|----------|---------|
| **A — Happy path** | Both participants vote PREPARED → coordinator commits → both apply the change |
| **B — Veto abort** | `ledger-debit` votes ABORT → coordinator aborts everyone → no state change |

The demo runs both back-to-back so you can see the commit *and* the proof that a vetoed transaction leaves no side effects.

## Layout

```
go.mod
main.go      participant servers + driver — all in one file for readability
```

## Run It

Requires a live Djinn on port `9004`.

```bash
mise r demo-txn                          # from repo root
# or
go run . [djinn-host:port]               # default localhost:9004
```

## Build

```bash
go build -o bin/double-entry .

# from the repo root
make build-examples
```

## Notes

- Participant servers bind to local ports and the Djinn dials them — when running the Djinn in Docker against host participants on Linux, the compose file sets `extra_hosts: host.docker.internal:host-gateway` for exactly this case
