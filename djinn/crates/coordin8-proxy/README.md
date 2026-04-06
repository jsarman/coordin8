# coordin8-proxy

Smart Proxy — TCP forwarding with live failover. Consumers ask for a capability template; the proxy hands back a local port that forwards to whichever service currently matches.

## Where It Fits

Layer 3 (port `9003`). Depends on the [Registry](../coordin8-registry/README.md) — every forwarded TCP connection re-resolves the upstream at connect time, so when a service moves or fails over, the **next** connection finds the new one without the consumer doing anything.

## Layout

```
src/
  lib.rs       re-exports ProxyManager, ProxyConfig, ProxyServiceImpl
  manager.rs   ProxyManager — owns the listener pool, resolves templates per-connection
  service.rs   tonic server impl for ProxyService (OpenProxy, Release)
```

## Configuration

`ProxyConfig::from_env()` reads:

| Env var            | Default      | Purpose |
|--------------------|--------------|---------|
| `PROXY_BIND_HOST`  | `127.0.0.1`  | bind address for forwarded ports — set to `0.0.0.0` in containers |
| `PROXY_PORT_MIN`   | unset        | low end of fixed port range |
| `PROXY_PORT_MAX`   | unset        | high end of fixed port range |

When the range is unset the OS picks an ephemeral port. The Docker compose stack pins `9100-9200`.

## Build / Test

```bash
cargo build -p coordin8-proxy
cargo test  -p coordin8-proxy
```

## Notes

- The proxy proto uses `Release`, not `Close` — `close` collides with the gRPC base `Client.close()` in Node's `@grpc/grpc-js`
- Port range exhaustion returns an error to the caller; pick a wide enough range for the expected concurrent forwards
- This crate does **not** speak the application protocol — it forwards bytes, period. Consumers terminate gRPC / HTTP / whatever themselves
