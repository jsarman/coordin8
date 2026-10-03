/* eslint-disable @typescript-eslint/no-explicit-any */
import { afterEach, describe, it } from "node:test";
import assert from "node:assert/strict";
import * as grpc from "@grpc/grpc-js";
import { Capability, RegistryServiceService } from "../gen/coordin8/registry";
import { SpaceServiceService, WriteResponse } from "../gen/coordin8/space";
import { DjinnClient } from "../src/index";
import { Cleanup, grpcError, serve } from "./helpers";

const cleanup = new Cleanup();
afterEach(() => cleanup.run());

type Entry = { host?: string; port?: string } | "no-transport";

async function startRegistry(entries: Record<string, Entry>) {
  const lookups: string[] = [];
  const auth: string[][] = [];
  const server = cleanup.server(
    await serve([
      [
        RegistryServiceService,
        {
          lookup: (call, cb) => {
            auth.push(call.metadata.get("authorization").map(String));
            const iface = call.request.template.interface;
            lookups.push(iface);
            const e = entries[iface];
            if (!e) return cb(grpcError(grpc.status.NOT_FOUND));
            const transport =
              e === "no-transport" ? undefined : { type: "grpc", config: { ...e } as Record<string, string> };
            cb(null, { capabilityId: `${iface}-id`, interface: iface, attrs: {}, transport });
          },
        },
        { lookup: Capability },
      ],
    ])
  );
  return { server, lookups, auth };
}

async function startSpace() {
  const state = { writes: 0, auth: [] as string[][] };
  const server = cleanup.server(
    await serve([
      [
        SpaceServiceService,
        {
          write: (call, cb) => {
            state.writes++;
            state.auth.push(call.metadata.get("authorization").map(String));
            cb(null, { tuple: { tupleId: "t", attrs: call.request.attrs, payload: Buffer.alloc(0) } });
          },
        },
        { write: WriteResponse },
      ],
    ])
  );
  return { server, state };
}

describe("DjinnClient.connect", () => {
  it("resolves Proxy, Space and EventMgr through Registry", async () => {
    const space = await startSpace();
    // Registry knows Space at the fake space server's real port.
    const r = await startRegistry({
      Proxy: { host: "127.0.0.1", port: "1" },
      Space: { host: "127.0.0.1", port: String(space.server.port) },
      EventMgr: { host: "127.0.0.1", port: "2" },
    });

    const djinn = cleanup.add(await DjinnClient.connect(r.server.addr), (d) => d.close());
    assert.deepEqual([...r.lookups].sort(), ["EventMgr", "Proxy", "Space"]);
    const t = await djinn.space().write({ attrs: { a: "b" }, ttlSeconds: 10 });
    assert.equal(t.tupleId, "t");
    assert.equal(space.state.writes, 1);
  });

  it("pinned addresses skip the Registry lookup", async () => {
    const space = await startSpace();
    const r = await startRegistry({
      Proxy: { host: "127.0.0.1", port: "1" },
      EventMgr: { host: "127.0.0.1", port: "2" },
    });
    const djinn = cleanup.add(
      await DjinnClient.connect(r.server.addr, { spaceAddr: space.server.addr }),
      (d) => d.close()
    );
    assert.deepEqual([...r.lookups].sort(), ["EventMgr", "Proxy"]);
    await djinn.space().write({ attrs: {}, ttlSeconds: 5 });
    assert.equal(space.state.writes, 1);
  });

  it("rejects when a service is missing from Registry", async () => {
    const r = await startRegistry({ Proxy: { host: "h", port: "1" } });
    await assert.rejects(DjinnClient.connect(r.server.addr), /look up (Space|EventMgr): not found in registry/);
  });

  it("rejects when the transport lacks host/port", async () => {
    const r = await startRegistry({
      Proxy: { host: "h" },
      Space: { host: "h", port: "1" },
      EventMgr: { host: "h", port: "1" },
    });
    await assert.rejects(DjinnClient.connect(r.server.addr), /look up Proxy: missing host\/port/);
  });

  it("rejects when the entry has no transport at all", async () => {
    const r = await startRegistry({
      Proxy: "no-transport",
      Space: "no-transport",
      EventMgr: "no-transport",
    });
    await assert.rejects(DjinnClient.connect(r.server.addr), /missing host\/port/);
  });

  it("attaches the token to every connection", async () => {
    const space = await startSpace();
    const r = await startRegistry({
      Proxy: { host: "127.0.0.1", port: "1" },
      Space: { host: "127.0.0.1", port: String(space.server.port) },
      EventMgr: { host: "127.0.0.1", port: "2" },
    });
    const djinn = cleanup.add(await DjinnClient.connect(r.server.addr, { token: "jwt-abc" }), (d) => d.close());
    await djinn.space().write({ attrs: {}, ttlSeconds: 5 });
    assert.equal(r.auth.length, 3);
    assert.ok(r.auth.every((h) => h.length === 1 && h[0] === "Bearer jwt-abc"));
    assert.deepEqual(space.state.auth, [["Bearer jwt-abc"]]);
  });

  it("sends no authorization header without a token", async () => {
    const r = await startRegistry({
      Proxy: { host: "h", port: "1" },
      Space: { host: "h", port: "1" },
      EventMgr: { host: "h", port: "1" },
    });
    cleanup.add(await DjinnClient.connect(r.server.addr), (d) => d.close());
    assert.ok(r.auth.every((h) => h.length === 0));
  });
});
