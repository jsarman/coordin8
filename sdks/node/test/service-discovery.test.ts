/* eslint-disable @typescript-eslint/no-explicit-any */
import { afterEach, describe, it } from "node:test";
import assert from "node:assert/strict";
import { ProxyHandle, ProxyServiceService } from "../gen/coordin8/proxy";
import * as grpc from "@grpc/grpc-js";
import { Lease, LeaseServiceService } from "../gen/coordin8/lease";
import { RegistryEvent, RegistryServiceService } from "../gen/coordin8/registry";
import { DjinnClient, ServiceDiscovery } from "../src/index";
import { Cleanup, grpcError, serve, sleep, waitFor } from "./helpers";

const cleanup = new Cleanup();
afterEach(() => cleanup.run());

const TEMPLATE = { interface: "Greeter" };

interface StartOpts {
  /** Lease TTL (seconds) the fake Proxy grants; 0/undefined = no lease. */
  leaseTtl?: number;
  /** Decides each Renew's outcome (1-based call number); return a status code to fail. */
  renewStatus?: (n: number) => grpc.status | undefined;
}

async function start(opts: StartOpts = {}) {
  const watchers: any[] = [];
  const proxy = {
    opens: 0,
    released: [] as string[],
    templates: [] as Array<Record<string, string>>,
    ttls: [] as number[],
    renewed: [] as string[],
  };
  const server = cleanup.server(
    await serve([
      [
        RegistryServiceService,
        {
          // Streams stay open so tests can push events.
          watch: (call) => {
            watchers.push(call);
          },
        },
        { watch: RegistryEvent },
      ],
      [
        ProxyServiceService,
        {
          open: (call, cb) => {
            proxy.templates.push(call.request.template);
            proxy.ttls.push(call.request.ttlSeconds);
            proxy.opens++;
            const n = proxy.opens;
            cb(null, {
              proxyId: `proxy-${n}`,
              localPort: 40000 + n,
              lease: opts.leaseTtl
                ? { leaseId: `please-${n}`, resourceId: `proxy-${n}`, ttlSeconds: opts.leaseTtl }
                : undefined,
            });
          },
          release: (call, cb) => {
            proxy.released.push(call.request.proxyId);
            cb(null, {});
          },
        },
        { open: ProxyHandle },
      ],
      [
        LeaseServiceService,
        {
          renew: (call, cb) => {
            proxy.renewed.push(call.request.leaseId);
            const code = opts.renewStatus?.(proxy.renewed.length);
            if (code !== undefined) return cb(grpcError(code));
            cb(null, { leaseId: call.request.leaseId, ttlSeconds: call.request.ttlSeconds });
          },
        },
        { renew: Lease },
      ],
    ])
  );
  const djinn = await DjinnClient.connect(server.addr, {
    proxyAddr: server.addr,
    spaceAddr: server.addr,
    eventAddr: server.addr,
  });
  cleanup.add(djinn, (d) => d.close());
  const discovery = ServiceDiscovery.watch(djinn);
  cleanup.add(discovery, (d) => d.close());
  const push = (type: number) => {
    for (const w of watchers) {
      w.write({ type, capability: { capabilityId: "c", interface: "Greeter", attrs: {} } });
    }
  };
  return { discovery, djinn, proxy, watchers, push };
}

describe("ServiceDiscovery", () => {
  it("repeated get with the same template reuses the cached proxy", async () => {
    const { discovery, proxy, watchers } = await start();
    const a = await discovery.get((addr) => addr, TEMPLATE);
    const b = await discovery.get((addr) => addr, { interface: "Greeter" });
    assert.equal(a, "localhost:40001");
    assert.equal(b, a);
    assert.equal(proxy.opens, 1);
    assert.deepEqual(proxy.templates[0], TEMPLATE);
    assert.ok(await waitFor(() => watchers.length === 1));
  });

  it("template key is independent of property order", async () => {
    const { discovery, proxy } = await start();
    const a = await discovery.get((addr) => addr, { interface: "Greeter", lang: "en" });
    const b = await discovery.get((addr) => addr, { lang: "en", interface: "Greeter" });
    assert.equal(a, b);
    assert.equal(proxy.opens, 1);
  });

  it("different templates get separate proxies", async () => {
    const { discovery, proxy } = await start();
    const a = await discovery.get((addr) => addr, TEMPLATE);
    const b = await discovery.get((addr) => addr, { interface: "Other" });
    assert.notEqual(a, b);
    assert.equal(proxy.opens, 2);
  });

  it("expired then registered keeps the live proxy: no new open, same address", async () => {
    const { discovery, proxy, watchers, push } = await start();
    const held = await discovery.get((addr) => addr, TEMPLATE);
    assert.ok(await waitFor(() => watchers.length >= 1));

    push(1 /* EXPIRED */);
    push(0 /* REGISTERED */);
    await sleep(300);
    assert.equal(await discovery.get((addr) => addr, TEMPLATE), held);
    assert.equal(proxy.opens, 1);
    assert.deepEqual(proxy.released, []);
  });

  it("at most one live proxy per template across expire/register cycles", async () => {
    const { discovery, proxy, watchers, push } = await start();
    await discovery.get((addr) => addr, TEMPLATE);
    assert.ok(await waitFor(() => watchers.length >= 1));
    for (let i = 0; i < 6; i++) {
      push(1 /* EXPIRED */);
      push(0 /* REGISTERED */);
      await sleep(50);
      await discovery.get((addr) => addr, TEMPLATE);
    }
    await sleep(200);
    assert.equal(proxy.opens - proxy.released.length, 1, `opens=${proxy.opens} released=${proxy.released}`);
  });

  it("registered while fresh does nothing", async () => {
    const { discovery, proxy, watchers, push } = await start();
    await discovery.get((addr) => addr, TEMPLATE);
    assert.ok(await waitFor(() => watchers.length >= 1));
    push(0);
    await sleep(300);
    assert.equal(proxy.opens, 1);
    assert.deepEqual(proxy.released, []);
  });

  it("close releases every cached proxy and cancels watches", async () => {
    const { discovery, proxy } = await start();
    await discovery.get((addr) => addr, TEMPLATE);
    await discovery.get((addr) => addr, { interface: "Other" });
    await discovery.close();
    assert.deepEqual([...proxy.released].sort(), ["proxy-1", "proxy-2"]);
  });

  it("modified neither releases nor reopens the proxy a caller is using", async () => {
    const { discovery, proxy, watchers, push } = await start();
    const held = await discovery.get((addr) => addr, TEMPLATE);
    assert.ok(await waitFor(() => watchers.length >= 1));
    push(2 /* MODIFIED */);
    await sleep(300);
    assert.equal(proxy.opens, 1);
    assert.deepEqual(proxy.released, []);
    assert.equal(await discovery.get((addr) => addr, TEMPLATE), held);
  });
});

describe("Proxy lease keep-alive", () => {
  it("open sends a TTL and the handle exposes the lease", async () => {
    const { djinn, proxy } = await start({ leaseTtl: 30 });
    const h = await djinn.proxy().open(TEMPLATE, 45);
    assert.deepEqual(proxy.ttls, [45]);
    assert.equal(h.lease?.leaseId, "please-1");
    assert.equal(h.isLost(), false);
    await djinn.proxy().release(h.proxyId);
    const h2 = await djinn.proxy().open(TEMPLATE);
    assert.equal(proxy.ttls[1], 30);
    await djinn.proxy().release(h2.proxyId);
  });

  it("renews periodically and stops on release", async () => {
    const { djinn, proxy } = await start({ leaseTtl: 1 }); // renews every 500ms
    const h = await djinn.proxy().open(TEMPLATE);
    assert.ok(await waitFor(() => proxy.renewed.length >= 2), "periodic renewals");
    assert.ok(proxy.renewed.every((id) => id === "please-1"));
    await djinn.proxy().release(h.proxyId);
    await sleep(100);
    const n = proxy.renewed.length;
    await sleep(1200);
    assert.equal(proxy.renewed.length, n, "no renewals after release");
    assert.equal(h.isLost(), false);
  });

  it("renew NOT_FOUND marks the discovery entry stale; next get reopens", async () => {
    const { discovery, proxy } = await start({
      leaseTtl: 1,
      renewStatus: () => grpc.status.NOT_FOUND,
    });
    const first = await discovery.get((addr) => addr, TEMPLATE);
    assert.equal(first, "localhost:40001");
    let second = first;
    assert.ok(
      await waitFor(() => {
        discovery.get((addr) => addr, TEMPLATE).then((a) => (second = a));
        return proxy.opens >= 2;
      }, 6000)
    );
    await waitFor(() => second !== first);
    assert.equal(second, "localhost:40002");
    // The lost proxy is released on replacement.
    assert.ok(proxy.released.includes("proxy-1"));
  });

  it("a transient renew error (UNAVAILABLE) keeps renewing", async () => {
    const { djinn, proxy } = await start({
      leaseTtl: 1,
      renewStatus: (n) => (n <= 2 ? grpc.status.UNAVAILABLE : undefined),
    });
    const h = await djinn.proxy().open(TEMPLATE);
    assert.ok(await waitFor(() => proxy.renewed.length >= 4, 6000), "renewals continue past errors");
    assert.equal(h.isLost(), false);
    await djinn.proxy().release(h.proxyId);
  });
});
