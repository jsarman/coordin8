# PRD: ServiceDiscovery — Jini-Inspired One-Liner Client

## Problem

Even with the smart proxy, clients still need to explicitly call `openProxy` or
`proxyStub` with a factory each time they want a service. That's one call too many.

The Jini `ServiceDiscoveryManager` set the bar: get a manager once at startup,
then call `.get()` anywhere in your app. The manager handles everything — proxy
lifecycle, caching, watch-based refresh, transparent reconnection.

## Goal

```java
// Java — two lines total. Second line is your actual work.
var discovery = ServiceDiscovery.watch(djinn);
var greeter   = discovery.get(GreeterServiceGrpc::newBlockingStub, Map.of("interface", "Greeter"));
```

```typescript
// Node
const discovery = ServiceDiscovery.watch(djinn);
const greeter = await discovery.get(
  addr => new GreeterServiceClient(addr, creds),
  { interface: "Greeter" }
);
```

```go
// Go
discovery := coordin8.NewServiceDiscovery(djinn)
conn, cleanup := discovery.Get(ctx, coordin8.Template{"interface": "Greeter"})
stub := gen.NewGreeterServiceClient(conn)
```

## Design

### ServiceDiscovery Manager

A per-app singleton (or scoped to a DjinnClient) that:

1. **Caches proxies by template** — ask for `{"interface": "Greeter"}` twice,
   get back the same underlying proxy port. No duplicate proxy handles.

2. **Watches the registry** — subscribes to `Registry.Watch` for each unique
   template. When a matching service departs and re-registers, the manager
   closes the stale proxy and opens a fresh one automatically. In-progress calls
   drain on the old connection; new calls go to the new one.

3. **Returns ready stubs** — `.get()` takes a factory, opens/reuses a proxy,
   calls the factory with `localhost:<port>`, returns the stub. One call.

4. **Graceful shutdown** — `.close()` releases all proxies and watch streams.

### Cache key

Template map serialized to a canonical sorted string. Same logical query always
hits the same cache entry.

### Watch behavior

Each unique template gets one `Registry.Watch` stream. On `EXPIRED` event:
- Mark cached proxy as stale
- Next `.get()` call re-opens a fresh proxy (lazy refresh)

On `REGISTERED` event while stale:
- Eagerly re-open the proxy so the next caller doesn't pay the latency

### SDK implementations

**All three languages** get a `ServiceDiscovery` class/struct with:
- `watch(djinn)` / `NewServiceDiscovery(djinn)` — constructor, starts watching
- `get(factory, template)` — returns cached-or-fresh stub
- `close()` — cleanup

No proto changes. No Djinn changes. Pure SDK layer on top of what we have.

## Deliverables

| # | Item |
|---|------|
| 1 | Go: `coordin8.ServiceDiscovery` with `Get`, `Close` |
| 2 | Java: `ServiceDiscovery.watch(djinn)` with `get`, `close` |
| 3 | Node: `ServiceDiscovery.watch(djinn)` with `get`, `close` |
| 4 | Updated examples — one `ServiceDiscovery.watch` at top, one `.get` call |
| 5 | Commit |

## Out of Scope

- Blocking `.get()` until a service appears (wait/retry on NOT_FOUND)
- Load balancing across multiple matching capabilities
- Per-stub health checking
