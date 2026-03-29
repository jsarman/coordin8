# PRD: Smart Proxy — Language-Agnostic Service Connection

## Problem

Every Coordin8 client — regardless of language — performs the same three boilerplate steps before it can call a service:

1. `registry.lookup(template)` — find the capability
2. Extract `host:port` from the transport descriptor
3. Build a channel/connection to that address

Only step 4 (creating a typed stub) and step 5 (calling the method) are the developer's actual concern. Steps 1-3 are infrastructure tax paid in every language, every time.

## Goal

A developer should be able to go from "I want a Greeter" to a working, typed stub in **one call**, in any language, without writing a single line of registry or transport logic.

```typescript
// Before
const cap = await djinn.registry().lookup({ interface: "Greeter" });
const host = cap.transport?.config["host"];
const port = cap.transport?.config["port"];
const channel = new grpc.Channel(`${host}:${port}`, creds, {});
const greeter = new GreeterServiceClient(`${host}:${port}`, creds);

// After
const greeter = await djinn.proxyClient(GreeterServiceClient, { interface: "Greeter" });
```

## Design

### TCP Forwarding Proxy in the Djinn

The Djinn runs a ProxyService (port 9003) that manages local TCP forwarding ports. When a client requests a proxy:

1. Djinn looks up the matching capability in the registry
2. Allocates a local port (OS-assigned, ephemeral)
3. Starts a TCP listener on that port
4. Forwards all incoming connections to the discovered service host:port
5. Returns the local port to the caller

The client connects to `localhost:<localPort>` using its normal gRPC stubs. The Djinn handles all routing. If the service re-registers at a new address, new connections route to the new target automatically.

**Why TCP (not gRPC)?**
A TCP forwarder is protocol-agnostic — it works for gRPC, HTTP, anything. It requires zero proto knowledge at the proxy layer. Connection setup is the only time routing occurs; in-flight streams are not interrupted by re-registration.

### New Proto: `proxy.proto`

```protobuf
syntax = "proto3";
package coordin8;

import "google/protobuf/empty.proto";

service ProxyService {
    rpc Open(OpenRequest)    returns (ProxyHandle);
    rpc Close(CloseRequest)  returns (google.protobuf.Empty);
}

message OpenRequest {
    map<string, string> template = 1;
}

message ProxyHandle {
    string proxy_id   = 1;
    int32  local_port = 2;
}

message CloseRequest {
    string proxy_id = 1;
}
```

### New Rust Crate: `coordin8-proxy`

- `ProxyManager` — owns a map of `proxy_id → (listener, target_addr)`
- `open(template)` — registry lookup, bind OS port, spawn forwarder task, return handle
- `close(proxy_id)` — stop listener, clean up
- `forward_loop` — tokio task per proxy: accepts TCP connections, resolves current target, bidirectional copy

Boots after Registry. Port 9003.

### SDK Updates (all three languages)

Each SDK gets two additions to `DjinnClient`:

**Low-level (transport-only):**
```
openProxy(template)  → ProxyHandle { proxyId, localPort }
closeProxy(proxyId)  → void
```

**High-level one-liner:**
```typescript
// Node
proxyClient<T>(ClientClass, template): Promise<T>

// Java
proxyStub<T>(Function<ManagedChannel, T> factory, Map<String,String> template): T

// Go
ProxyClient[T](factory func(grpc.ClientConnInterface) T, template map[string]string) (T, func())
```

The high-level helper calls `openProxy`, builds `localhost:localPort` channel, and passes it to the factory. It also wires cleanup (close channel + close proxy) into the returned handle or defer.

### Boot Sequence Update

```
Provider → LeaseMgr → Space+EventMgr → TransactionMgr → Registry → Proxy
```

ProxyService depends on RegistryClient to resolve templates. It's the last service to boot.

## Deliverables

| # | Item | Notes |
|---|------|-------|
| 1 | `proto/coordin8/proxy.proto` | New service definition |
| 2 | `djinn/crates/coordin8-proxy` | Rust TCP forwarder crate |
| 3 | Djinn boot sequence | Wire ProxyService, port 9003 |
| 4 | Go SDK: `openProxy`, `ProxyClient` | Generic helper |
| 5 | Java SDK: `openProxy`, `proxyStub` | Method reference factory |
| 6 | Node SDK: `openProxy`, `proxyClient` | Constructor factory |
| 7 | Proto regen (Go + Node + Java) | All three gen targets updated |
| 8 | Updated examples | Before/after in each language |
| 9 | Makefile, .gitignore, commit | Clean close |

## Out of Scope

- Proxy-level load balancing (multiple matches → pick one, same as `lookup`)
- Watch-based live re-route of existing connections (new connections re-resolve, in-flight are not interrupted)
- Auth / mTLS at the proxy layer
- Python, Rust, or other language SDKs
