# coordin8-registry

Registry — attribute-based service discovery. Find services by *what they are*, not where they live.

## Where It Fits

Layer 2a (port `9002`). Each entry is paired with a lease from [`coordin8-lease`](../coordin8-lease/README.md) — when the lease expires, the entry is unregistered and an `EXPIRED` event is broadcast on the registry change stream. The Smart Proxy ([`coordin8-proxy`](../coordin8-proxy/README.md)) consumes the registry to resolve capability templates at connection time.

## Template Matching

Operators on attribute values:

| Operator        | Example                  | Matches |
|-----------------|--------------------------|---------|
| exact           | `language=english`       | only `english` |
| `contains:`     | `tags=contains:weather`  | any value containing `weather` |
| `starts_with:`  | `region=starts_with:us-` | `us-east-1`, `us-west-2`, ... |
| `Any` (missing) | (field omitted)          | matches anything |

The `interface` field is always injected into the match set alongside `attrs`.

## Layout

```
src/
  lib.rs       re-exports RegistryServiceImpl + module surface
  matcher.rs   template-matching engine (operators above)
  store.rs     RegistryIndex — wraps a RegistryStore with bookkeeping
  service.rs   tonic server impl + RegistryBroadcast change stream
```

## Build / Test

```bash
cargo build -p coordin8-registry
cargo test  -p coordin8-registry
```

## Notes

- Entries are keyed by `capability_id` (UUID); GSI on `lease_id` lets the expiry handler find the entry without a scan
- The change stream uses `tokio::sync::broadcast` with a buffer of 256 — slow consumers will lag and miss events
- `unregister_by_lease` is the hook the Djinn binary calls from the lease-expiry task
