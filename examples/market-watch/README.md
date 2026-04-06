# market-watch

Live demo of the Coordin8 [EventMgr](../../djinn/crates/coordin8-event/README.md). Proves durable event delivery end-to-end: subscribe, emit while no receiver is open, drain the mailbox on reconnect, then receive live.

For project context see the [root README](../../README.md).

## What It Does

1. Subscribe (DURABLE, TTL 60s) — gets back a leased registration
2. Emit 3 events while no `Receive` stream is open → events buffer in the mailbox
3. Open the `Receive` stream → drains the backlog in order, then stays live
4. Emit 2 more events while the stream is live → live delivery
5. Cancel the subscription

## Layout

```
go.mod
main.go      single-file Go program — all five steps in sequence
```

## Run It

Requires a live Djinn on port `9005`.

```bash
mise r demo-events                       # from repo root
# or
go run . [djinn-host:port]               # default localhost:9005
```

## Build

```bash
go build -o bin/market-watch .

# from the repo root
make build-examples
```
