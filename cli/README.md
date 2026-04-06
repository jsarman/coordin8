# coordin8 CLI

Go Cobra CLI for inspecting and interacting with a running Djinn.

For project context see the [root README](../README.md). This document covers the CLI directory only.

## Layout

```
go.mod                  module github.com/coordin8/cli (replace ../sdks/go)
cmd/coordin8/main.go    Cobra root + lease + registry subcommands
```

The CLI consumes the [Go SDK](../sdks/go/README.md) directly via a local `replace` in `go.mod`.

## Commands

```bash
coordin8 lease grant --resource worker-7 --ttl 30
coordin8 lease renew --lease <id>
coordin8 lease cancel --lease <id>
coordin8 lease watch

coordin8 registry register --interface Greeter --attrs language=english
coordin8 registry list
coordin8 registry lookup --match interface=Greeter
coordin8 registry watch  --match interface=WeatherStation
```

Global flag: `--host` (default `localhost`).

## Build

```bash
go build -o coordin8 ./cmd/coordin8/

# from the repo root, the Makefile target builds it next to the Djinn binary
make build       # writes djinn/coordin8
```

## Notes

- The CLI dials `--host` on the standard service ports (`9001` lease, `9002` registry)
- For remote Djinns, set `--host` to the host running the Djinn — the CLI does not currently support per-service host overrides
