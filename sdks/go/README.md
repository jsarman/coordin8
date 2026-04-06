# Coordin8 Go SDK

The Go client SDK for the Coordin8 Djinn. Thin wrapper over the tonic-generated gRPC stubs in `gen/`, plus a `ServiceDiscovery` helper that caches proxy connections by template and refreshes on lease expiry.

For project context see the [root README](../../README.md). This document covers the Go SDK directory only.

## Layout

```
go.mod                       module github.com/coordin8/sdk-go
gen/coordin8/                generated gRPC stubs (regenerate via `make proto` at repo root)
coordin8/
  client.go                  Client / Connect — owns gRPC ClientConns
  lease.go                   LeaseClient — Grant, Renew, Cancel, WatchExpiry
  registry.go                RegistryClient — Register, Lookup, List, Watch
  proxy.go                   ProxyClient — OpenProxy / Release
  discovery.go               ServiceDiscovery — cached, lease-aware lookups
```

## Public API

```go
djinn, err := coordin8.Connect("localhost")  // dials :9001 / :9002 / :9003
defer djinn.Close()

leases   := djinn.Leases()
registry := djinn.Registry()

// One-liner discovery — caches by template, refreshes on lease expiry
discovery, _ := coordin8.Watch(djinn)
greeter := pb.NewGreeterClient(discovery.Get(coordin8.Template{"interface": "Greeter"}))
```

## Build / Test

```bash
go build ./...
go test  ./...

# from the repo root
mise r test-go     # equivalent
```

## Regenerating Stubs

```bash
# from repo root
make proto         # regenerates Go, Node, and example stubs
```

## Notes

- `Template` is a `map[string]string` — operators (`contains:`, `starts_with:`) live in the value
- `ServiceDiscovery.Get` returns a `*grpc.ClientConn` you wrap in your generated client constructor
- The CLI ([`../../cli/`](../../cli/README.md)) uses this SDK via a `replace` directive in `cli/go.mod`
