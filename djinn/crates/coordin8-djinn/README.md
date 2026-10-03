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
| `COORDIN8_SHUTDOWN_GRACE_SECS`   | `20`    | graceful-shutdown budget (see below) |
| `RUST_LOG`                       | `coordin8=info` | tracing filter |

## Graceful shutdown

On SIGTERM or SIGINT the Djinn (bundled and every split subcommand) will:
flip its gRPC health status to NOT_SERVING (split mode), cancel its own
Registry self-registrations so clients stop resolving it immediately rather
than after the 30s self-lease TTL, stop accepting connections, drain in-flight
RPCs (including 2PC commits), and exit 0. The whole sequence is bounded by
`COORDIN8_SHUTDOWN_GRACE_SECS` (default `20`, kept under the usual 30s
Docker/k8s grace period); if RPCs are still in flight when it elapses the
process logs an error and exits non-zero. Long-lived server streams
(Registry `Watch`, `LeaseService.WatchExpiry`, EventMgr `Receive`, Space
`Notify`) are ended when draining starts, with `UNAVAILABLE` ("server shutting
down") so client reconnect loops move on rather than stalling the drain.

Docker stops a container after 10s by default, so the compose files set
`stop_grace_period: 25s` on the Djinn services (above the 20s default grace;
the k8s default `terminationGracePeriodSeconds` is 30s). If you lower the
Docker/k8s timeout, lower `COORDIN8_SHUTDOWN_GRACE_SECS` to stay under it.

## Notes

- Boot order is non-negotiable. Don't reorder layers.
- Each gRPC service binds its own `0.0.0.0:<port>` listener — they are joined with `tokio::try_join!`
- The Space port (9006) hosts both `SpaceServiceServer` and `ParticipantServiceServer` (2PC hook)
