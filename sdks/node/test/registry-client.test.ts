/* eslint-disable @typescript-eslint/no-explicit-any */
import { afterEach, describe, it } from "node:test";
import assert from "node:assert/strict";
import * as grpc from "@grpc/grpc-js";
import { Capability, RegisterResponse, RegistryEvent, RegistryServiceService } from "../gen/coordin8/registry";
import { RegistryClient } from "../src/index";
import { Cleanup, grpcError, insecureChannel, serve, waitFor } from "./helpers";

const cleanup = new Cleanup();
afterEach(() => cleanup.run());

const cap = (id: string, iface: string, attrs: Record<string, string> = {}) => ({
  capabilityId: id,
  interface: iface,
  attrs,
  transport: { type: "grpc", config: { host: "h", port: "1" } },
});

async function start() {
  const state = {
    lastRegister: undefined as any,
    lastModify: undefined as any,
    lastLookup: undefined as any,
    lastWatch: undefined as any,
    all: [] as any[],
    events: [] as any[],
    watchError: undefined as grpc.status | undefined,
    registerWithoutLease: false,
  };
  const server = cleanup.server(
    await serve([
      [
        RegistryServiceService,
        {
          register: (call, cb) => {
            state.lastRegister = call.request;
            cb(null, {
              capabilityId: call.request.capabilityId || "cap-new",
              lease: state.registerWithoutLease
                ? undefined
                : { leaseId: "lease-1", ttlSeconds: Number(call.request.ttlSeconds) / 2 },
            });
          },
          modifyAttrs: (call, cb) => {
            state.lastModify = call.request;
            cb(null, cap(call.request.capabilityId, "Svc", { modified: "yes" }));
          },
          lookup: (call, cb) => {
            state.lastLookup = call.request;
            if ("boom" in call.request.template) return cb(grpcError(grpc.status.INTERNAL));
            if (state.all.length === 0) return cb(grpcError(grpc.status.NOT_FOUND));
            cb(null, state.all[0]);
          },
          lookupAll: (call) => {
            state.lastLookup = call.request;
            state.all.forEach((c) => call.write(c));
            call.end();
          },
          watch: (call) => {
            state.lastWatch = call.request;
            state.events.forEach((e) => call.write(e));
            if (state.watchError !== undefined) call.emit("error", grpcError(state.watchError));
            else call.end();
          },
        },
        {
          register: RegisterResponse,
          modifyAttrs: Capability,
          lookup: Capability,
          lookupAll: Capability,
          watch: RegistryEvent,
        },
      ],
    ])
  );
  const channel = cleanup.add(insecureChannel(server.addr), (c) => c.close());
  return { state, client: new RegistryClient(channel) };
}

describe("RegistryClient", () => {
  it("register sends all fields and maps the result", async () => {
    const { state, client } = await start();
    const r = await client.register(
      "Greeter",
      { a: "b" },
      { type: "grpc", config: { host: "x", port: "9" } },
      20,
      "cap-7"
    );
    const req = state.lastRegister;
    assert.equal(req.interface, "Greeter");
    assert.deepEqual(req.attrs, { a: "b" });
    assert.equal(Number(req.ttlSeconds), 20);
    assert.equal(req.transport.type, "grpc");
    assert.deepEqual(req.transport.config, { host: "x", port: "9" });
    assert.equal(req.capabilityId, "cap-7");
    assert.deepEqual(r, { capabilityId: "cap-7", leaseId: "lease-1", grantedTtlSeconds: 10 });
  });

  it("register without transport/capabilityId/lease uses defaults", async () => {
    const { state, client } = await start();
    state.registerWithoutLease = true;
    const r = await client.register("G", {}, undefined, 10);
    assert.equal(state.lastRegister.transport, undefined);
    assert.equal(state.lastRegister.capabilityId, "");
    assert.deepEqual(r, { capabilityId: "cap-new", leaseId: "", grantedTtlSeconds: 0 });
  });

  it("modifyAttrs sends add/remove and tolerates omissions", async () => {
    const { state, client } = await start();
    const rec = await client.modifyAttrs("c1", { k: "v" }, ["gone"]);
    assert.equal(state.lastModify.capabilityId, "c1");
    assert.deepEqual(state.lastModify.addAttrs, { k: "v" });
    assert.deepEqual(state.lastModify.removeAttrs, ["gone"]);
    assert.equal(rec.capabilityId, "c1");
    assert.deepEqual(rec.attrs, { modified: "yes" });

    await client.modifyAttrs("c2");
    assert.deepEqual(state.lastModify.addAttrs, {});
    assert.deepEqual(state.lastModify.removeAttrs, []);
  });

  it("lookup maps capability and transport", async () => {
    const { state, client } = await start();
    state.all = [cap("c1", "Greeter", { lang: "en" })];
    const r = await client.lookup({ interface: "Greeter" });
    assert.deepEqual(state.lastLookup.template, { interface: "Greeter" });
    assert.equal(r?.capabilityId, "c1");
    assert.equal(r?.interfaceName, "Greeter");
    assert.deepEqual(r?.attrs, { lang: "en" });
    assert.equal(r?.transport?.type, "grpc");
    assert.equal(r?.transport?.config.host, "h");
  });

  it("lookup returns null on NOT_FOUND but rejects on other errors", async () => {
    const { client } = await start();
    assert.equal(await client.lookup({ interface: "none" }), null);
    await assert.rejects(client.lookup({ boom: "1" }), (e: grpc.ServiceError) => e.code === grpc.status.INTERNAL);
  });

  it("lookupAll collects the stream (and is empty when nothing matches)", async () => {
    const { state, client } = await start();
    assert.deepEqual(await client.lookupAll({ interface: "G" }), []);
    state.all = [cap("a", "G"), cap("b", "G"), cap("c", "G")];
    const r = await client.lookupAll({ interface: "G" });
    assert.deepEqual(r.map((c) => c.capabilityId), ["a", "b", "c"]);
  });

  it("watch maps event types", async () => {
    const { state, client } = await start();
    state.events = [
      { type: 0, capability: cap("a", "G") },
      { type: 1, capability: cap("b", "G") },
      { type: 2, capability: cap("c", "G") },
      { type: 1 },
    ];
    const got: Array<{ type: string; capability?: { capabilityId: string } }> = [];
    const stream = client.watch({ interface: "G" }, (e) => got.push(e), () => {});
    assert.ok(await waitFor(() => got.length === 4));
    stream.cancel();
    assert.deepEqual(state.lastWatch.template, { interface: "G" });
    assert.deepEqual(got.map((e) => e.type), ["registered", "expired", "modified", "expired"]);
    assert.equal(got[0].capability?.capabilityId, "a");
    assert.equal(got[3].capability, undefined);
  });

  it("watch reports stream errors via onError", async () => {
    const { state, client } = await start();
    state.watchError = grpc.status.UNAVAILABLE;
    const errs: grpc.ServiceError[] = [];
    client.watch({}, () => {}, (e) => errs.push(e as grpc.ServiceError));
    assert.ok(await waitFor(() => errs.length >= 1));
    assert.equal(errs[0].code, grpc.status.UNAVAILABLE);
  });
});
