# coordin8 CLI

Go Cobra CLI for inspecting and interacting with a running Djinn.

For project context see the [root README](../README.md). This document covers the CLI directory only.

## Layout

```
go.mod                  module github.com/coordin8/cli (replace ../sdks/go)
cmd/coordin8/main.go    Cobra root + lease + registry subcommands
cmd/coordin8/space.go   space (tuple store) subcommands
cmd/coordin8/auth.go    auth mint-token subcommand
```

The CLI consumes the [Go SDK](../sdks/go/README.md) directly via a local `replace` in `go.mod`.

## Commands

```bash
# lease — dials the grantor directly (see --grantor below), not the Registry
coordin8 lease grant  --resource worker-7 --ttl 30
coordin8 lease renew  --id <lease-id> --ttl 30
coordin8 lease cancel --id <lease-id>
coordin8 lease watch  [--resource worker-7]

# registry — dials --registry
coordin8 registry register --interface Greeter --attr language=english --ttl 60
coordin8 registry list
coordin8 registry lookup --match interface=Greeter
coordin8 registry watch  --match interface=WeatherStation

# space (tuple store) — dials --registry, discovers Space through it
coordin8 space write    --attr kind=task --payload '{"foo":1}' --ttl 60
coordin8 space read     --match kind=task [--wait --timeout 10]
coordin8 space take     --match kind=task [--wait --timeout 10]
coordin8 space contents [--match kind=task]
coordin8 space notify   --match kind=task --on appearance
coordin8 space cancel   --id <tuple-id>
coordin8 space renew    --id <tuple-id> --ttl 30

# auth — offline JWT minting, never dials a Djinn
COORDIN8_JWT_SECRET=... coordin8 auth mint-token --sub greeter --ttl 720h
```

Global flags: `--registry` (default `localhost:9002` — every service except a lease's own grantor is looked up through it) and `--token` (default `$COORDIN8_TOKEN`, for an auth-enabled Djinn).

`lease renew`/`cancel`/`watch` accept `--grantor host:port` to dial the specific Registry/EventMgr/Space/TransactionMgr replica that granted the lease, when it isn't the Registry itself (`dialLeaseGrantor` in `main.go` defaults to `--registry`).

## Build

```bash
go build -o coordin8 ./cmd/coordin8/

# from the repo root, the Makefile target builds it next to the Djinn binary
make build       # writes djinn/coordin8
```

## Notes

- There is no standalone LeaseMgr — leases are granted by whichever service you asked (Registry by default; pass `--grantor` for a Space- or EventMgr-granted lease). See the root README's "Split Mode" section for the full per-service port list.
- For remote Djinns, set `--registry` to that host's Registry address; per-service overrides go through `--grantor`.
