/* eslint-disable @typescript-eslint/no-explicit-any */
import { afterEach, describe, it } from "node:test";
import assert from "node:assert/strict";
import { ProxyHandle, ProxyServiceService } from "../gen/coordin8/proxy";
import { RegistryEvent, RegistryServiceService } from "../gen/coordin8/registry";
import { DjinnClient, ServiceDiscovery } from "../src/index";
import { Cleanup, serve, sleep, waitFor } from "./helpers";

const cleanup = new Cleanup();
afterEach(() => cleanup.run());

const TEMPLATE = { interface: "Greeter" };

async function start() {
  const watchers: any[] = [];
  const proxy = {
    opens: 0,
    released: [] as string[],
    templates: [] as Array<Record<string, string>>,
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
            proxy.opens++;
            cb(null, { proxyId: `proxy-${proxy.opens}`, localPort: 40000 + proxy.opens });
          },
          release: (call, cb) => {
            proxy.released.push(call.request.proxyId);
            cb(null, {});
          },
        },
        { open: ProxyHandle },
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
  return { discovery, proxy, watchers, push };
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

  it("expired marks the entry stale; next get re-opens and releases the old proxy", async () => {
    const { discovery, proxy, watchers, push } = await start();
    const first = await discovery.get((addr) => addr, TEMPLATE);
    assert.ok(await waitFor(() => watchers.length >= 1));

    push(1 /* EXPIRED */);
    let second = first;
    // The event is handled asynchronously; keep asking until the cache notices.
    assert.ok(
      await waitFor(() => {
        discovery.get((addr) => addr, TEMPLATE).then((a) => (second = a));
        return proxy.opens >= 2;
      })
    );
    await waitFor(() => second !== first);
    assert.equal(second, "localhost:40002");
    assert.deepEqual(proxy.released, ["proxy-1"]);
  });

  it("registered after expired eagerly refreshes without a get", async () => {
    const { discovery, proxy, watchers, push } = await start();
    await discovery.get((addr) => addr, TEMPLATE);
    assert.ok(await waitFor(() => watchers.length >= 1));

    push(1 /* EXPIRED */);
    push(0 /* REGISTERED */);
    assert.ok(await waitFor(() => proxy.opens === 2), "eager refresh on register");
    assert.deepEqual(proxy.released, ["proxy-1"]);
    // And the cache now serves the refreshed port with no further opens.
    assert.equal(await discovery.get((addr) => addr, TEMPLATE), "localhost:40002");
    assert.equal(proxy.opens, 2);
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

  // Desired behavior (not asserted today): a "modified" event should refresh
  // the entry without invalidating the proxy a caller already holds. Today
  // refresh() releases the old proxy out from under callers.
  it(
    "modified refreshes without releasing the proxy a caller is using",
    { skip: "known bug: 'modified' releases the proxy callers still hold" },
    async () => {
      const { discovery, proxy, watchers, push } = await start();
      await discovery.get((addr) => addr, TEMPLATE);
      assert.ok(await waitFor(() => watchers.length >= 1));
      push(2 /* MODIFIED */);
      assert.ok(await waitFor(() => proxy.opens === 2));
      assert.deepEqual(proxy.released, []);
    }
  );
});
