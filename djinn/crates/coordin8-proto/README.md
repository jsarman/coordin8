# coordin8-proto

Tonic-generated gRPC bindings for every Coordin8 service. This crate's job is to compile the `.proto` files in [`../../../proto/`](../../../proto/) into Rust types and `tonic` server/client stubs.

## Where It Fits

Every service crate (`coordin8-lease`, `coordin8-registry`, ...) imports its server trait from here. The Djinn binary imports the `*Server` types and registers them with `tonic::transport::Server`.

## Layout

```
build.rs    invokes tonic_build against ../../../proto/coordin8/*.proto
src/lib.rs  re-exports the generated module as `coordin8_proto::coordin8::*`
```

`build.rs` compiles:

- `lease.proto`
- `registry.proto`
- `proxy.proto`
- `event.proto`
- `transaction.proto`
- `space.proto`

`common.proto` is tracked via `cargo:rerun-if-changed` so edits trigger a rebuild even though it isn't compiled directly.

## Build

```bash
cargo build -p coordin8-proto
```

Stubs regenerate automatically whenever any `.proto` file changes — no manual `make proto` needed for the Rust side. (The `make proto` task at the repo root regenerates stubs for the **other** language SDKs.)

## Notes

- Both client and server code are generated (`build_client(true).build_server(true)`)
- Import paths look like `coordin8_proto::coordin8::lease_service_server::LeaseServiceServer`
- If you add a new `.proto` file, add it to the `protos` array in `build.rs`
