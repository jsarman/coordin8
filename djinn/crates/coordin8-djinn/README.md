# coordin8-djinn

The binary entry point. This crate is a single `main.rs` that picks a provider, builds every service in the correct order, and starts the gRPC servers.

## Where It Fits

This is the only crate in the workspace that pulls in concrete provider implementations. Every service crate depends on the abstract `*Store` traits in [`coordin8-core`](../coordin8-core/README.md); this binary chooses between [`coordin8-provider-local`](../../providers/local/README.md) and [`coordin8-provider-dynamo`](../../providers/dynamo/README.md) at startup.

## Boot Sequence

```
Layer 0   Provider               (local | dynamo)
Layer 1   Registry      :9002    (+ LeaseService)
Layer 1   EventMgr      :9005    (+ LeaseService)
Layer 1   Space         :9006    (+ LeaseService)
Layer 2   Proxy         :9003
Layer 3   TransactionMgr :9004   (+ LeaseService)
```

There's no standalone LeaseMgr — Registry, EventMgr, Space, and TransactionMgr each embed their own `LeaseManager` and mount `LeaseService` on their own port, which is why all three of Registry/EventMgr/Space sit at Layer 1 rather than depending on a shared bedrock service. Each service's own lease expirations are broadcast on its own `tokio::sync::broadcast` channel; Registry, EventMgr, Space, and TransactionMgr each spawn a task that subscribes to its own and routes by `resource_id` prefix (`registry:`, `event:`, `space:`, `space-watch:`, `txn:`).

## Layout

```
src/main.rs    boot sequence + tonic Server::builder wiring
```

## Build / Run

```bash
cargo build -p coordin8-djinn
cargo run   -p coordin8-djinn         # local provider, dev profile
cargo build --release                  # used by Dockerfile.djinn

# from the repo root
mise r djinn                           # equivalent to `cd djinn && cargo run`
```

## Configuration

| Env var                          | Default | Purpose |
|----------------------------------|---------|---------|
| `COORDIN8_PROVIDER`              | `local` | `local` or `dynamo` |
| `COORDIN8_AUTO_CREATE_TABLES`    | unset   | dynamo provider only — create tables on init |
| `DYNAMODB_ENDPOINT`              | unset   | dynamo provider only — override endpoint (MiniStack) |
| `COORDIN8_LEASE_MAX_TTL`         | unset   | maximum grantable lease TTL (seconds) |
| `COORDIN8_LEASE_PREFERRED_TTL`   | (set in core) | default suggested TTL |
| `PROXY_BIND_HOST`                | `127.0.0.1` | bind for forwarded ports — use `0.0.0.0` in containers |
| `PROXY_PORT_MIN` / `PROXY_PORT_MAX` | unset | fixed forwarded-port range |
| `RUST_LOG`                       | `coordin8=info` | tracing filter |

## Notes

- Boot order is non-negotiable. Don't reorder layers.
- Each gRPC service binds its own `0.0.0.0:<port>` listener — they are joined with `tokio::try_join!`
- The Space port (9006) hosts both `SpaceServiceServer` and `ParticipantServiceServer` (2PC hook)
