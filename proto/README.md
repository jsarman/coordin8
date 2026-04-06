# proto

The wire-protocol source of truth for Coordin8. Every SDK and the Djinn itself compile their gRPC stubs from these `.proto` files.

## Layout

```
coordin8/
  common.proto         shared messages (Attributes, Empty wrappers, ...)
  lease.proto          LeaseService — Grant, Renew, Cancel, WatchExpiry
  registry.proto       RegistryService — Register, Lookup, List, Watch
  proxy.proto          ProxyService — OpenProxy, Release
  event.proto          EventService — Subscribe, Emit, Receive, Cancel
  transaction.proto    TransactionService + ParticipantService (2PC)
  space.proto          SpaceService — out, read, take, watch
```

## Who Compiles What

| Consumer | How |
|----------|-----|
| Rust (Djinn) | [`djinn/crates/coordin8-proto/build.rs`](../djinn/crates/coordin8-proto/README.md) — automatic on every `cargo build` |
| Go SDK | `make proto` → `protoc` into `sdks/go/gen/` |
| Java SDK | `./gradlew build` — Gradle's `protobuf` plugin reads `srcDir '../../proto'` |
| Node SDK | `cd sdks/node && npm run proto` — `ts-proto` into `sdks/node/gen/` |

The repo-root `make proto` target regenerates **all** language stubs in one shot (Go SDK, Node SDK, and the example greeter stubs). Java and Rust regenerate during their normal build, so they are not in the Make target.

## Regenerate Everything

```bash
# from repo root
make proto
# or
mise r proto
```

## Gotchas

- `proxy.proto` uses `Release`, not `Close` — `close` collides with `Client.close()` in Node's `@grpc/grpc-js`
- `lease.proto` generates a Java outer class named `LeaseOuterClass`, not `Lease`. Protobuf renames the outer class when the filename collides with a message name.
- All services declare `option go_package = "github.com/coordin8/proto/coordin8";` — change this only if you also update the SDK module path
- When adding a new service, you must update **four** places: this directory, `djinn/crates/coordin8-proto/build.rs`, the `Makefile` `proto` target, and the Djinn binary's `main.rs` to wire the server
