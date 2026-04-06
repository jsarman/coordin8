# Coordin8 Node.js / TypeScript SDK

The Node SDK for the Coordin8 Djinn. Wraps the `ts-proto` generated stubs in `gen/` and exposes the same surface area as the Go and Java SDKs.

For project context see the [root README](../../README.md). This document covers the Node SDK only.

## Layout

```
package.json                  npm scripts (build, proto)
tsconfig.json                 rootDir = ".", outDir = "dist"
src/
  index.ts                    public exports
  djinn-client.ts             DjinnClient — owns the gRPC channels
  lease-client.ts             LeaseClient
  registry-client.ts          RegistryClient
  proxy-client.ts             ProxyClient
  service-discovery.ts        ServiceDiscovery — cached, lease-aware lookups
gen/                          generated ts-proto stubs
dist/src/                     tsc output (package main + types point here)
```

## Public API

```typescript
import { DjinnClient, ServiceDiscovery } from "@coordin8/sdk";

const djinn = await DjinnClient.connect("localhost");
const discovery = await ServiceDiscovery.watch(djinn);
const greeter = await discovery.get(
  addr => new GreeterClient(addr, creds),
  { interface: "Greeter" },
);
```

## Build / Test

```bash
npm install
npm run build         # tsc → dist/src
npm run proto         # regenerate ts-proto stubs from ../../proto
```

## Gotchas

- `tsconfig.json` has `rootDir: "."` so output lands in `dist/src/`. `package.json` `main` and `types` point to `dist/src/index.{js,d.ts}` — don't move these without updating both.
- Stubs are generated with `ts-proto` options `outputServices=grpc-js,esModuleInterop=true,env=node`
- `ProxyClient` and `ServiceDiscovery.get` use a `(address: string) => T` factory pattern — building the client inside the SDK would create cross-module `@grpc/grpc-js` credential identity mismatches
- `proxy.proto` uses `Release`, not `Close` — `close` collides with the gRPC base `Client.close()`
