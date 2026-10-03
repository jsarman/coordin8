/* eslint-disable @typescript-eslint/no-explicit-any */
import { afterEach, describe, it } from "node:test";
import assert from "node:assert/strict";
import { Event, EventRegistration, EventServiceService } from "../gen/coordin8/event";
import {
  ReadResponse,
  SpaceEvent,
  SpaceServiceService,
  TakeResponse,
  Tuple,
  WriteResponse,
} from "../gen/coordin8/space";
import { Lease } from "../gen/coordin8/lease";
import { EventClient, SpaceClient } from "../src/index";
import { Cleanup, insecureChannel, serve } from "./helpers";

const cleanup = new Cleanup();
afterEach(() => cleanup.run());

const tuple = (id: string) => ({
  tupleId: id,
  attrs: { k: "v" },
  payload: Buffer.from("pay"),
  lease: { leaseId: `lease-${id}` },
  provenance: { writtenBy: "me", writtenAt: new Date(50_000), inputTupleId: "in" },
});

async function startSpace() {
  const s = {
    write: undefined as any,
    read: undefined as any,
    take: undefined as any,
    contents: undefined as any,
    notify: undefined as any,
    cancelled: undefined as string | undefined,
    hasMatch: true,
  };
  const server = cleanup.server(
    await serve([
      [
        SpaceServiceService,
        {
          write: (call, cb) => {
            s.write = call.request;
            cb(null, { tuple: tuple("w") });
          },
          read: (call, cb) => {
            s.read = call.request;
            cb(null, s.hasMatch ? { tuple: tuple("r") } : {});
          },
          take: (call, cb) => {
            s.take = call.request;
            cb(null, s.hasMatch ? { tuple: tuple("t") } : {});
          },
          contents: (call) => {
            s.contents = call.request;
            call.write(tuple("c1"));
            call.write(tuple("c2"));
            call.end();
          },
          notify: (call) => {
            s.notify = call.request;
            call.write({ type: 1, tuple: tuple("n"), occurredAt: new Date(9_000), handback: call.request.handback });
            call.write({ type: 0, handback: Buffer.alloc(0) });
            call.end();
          },
          renew: (call, cb) => cb(null, { leaseId: `renewed-${call.request.tupleId}`, ttlSeconds: call.request.ttlSeconds }),
          cancel: (call, cb) => {
            s.cancelled = call.request.tupleId;
            cb(null, {});
          },
        },
        {
          write: WriteResponse,
          read: ReadResponse,
          take: TakeResponse,
          contents: Tuple,
          notify: SpaceEvent,
          renew: Lease,
        },
      ],
    ])
  );
  const client = new SpaceClient(cleanup.add(insecureChannel(server.addr), (c) => c.close()));
  return { s, client };
}

async function startEvents() {
  const e = {
    subscribe: undefined as any,
    emit: undefined as any,
    cancelled: undefined as string | undefined,
  };
  const server = cleanup.server(
    await serve([
      [
        EventServiceService,
        {
          subscribe: (call, cb) => {
            e.subscribe = call.request;
            cb(null, {
              registrationId: "reg-1",
              source: call.request.source,
              seqNum: 42,
              lease: { leaseId: "sub-lease" },
            });
          },
          receive: (call) => {
            call.write({
              eventId: "e1",
              source: "src",
              eventType: "tick",
              seqNum: 3,
              attrs: { a: "b" },
              payload: Buffer.from("p"),
              handback: Buffer.from("hb"),
              emittedAt: new Date(5_000),
            });
            call.write({ eventId: "e2", attrs: {}, payload: Buffer.alloc(0), handback: Buffer.alloc(0) });
            call.end();
          },
          emit: (call, cb) => {
            e.emit = call.request;
            cb(null, {});
          },
          renewSubscription: (call, cb) =>
            cb(null, { leaseId: call.request.registrationId, ttlSeconds: call.request.ttlSeconds }),
          cancelSubscription: (call, cb) => {
            e.cancelled = call.request.registrationId;
            cb(null, {});
          },
        },
        {
          subscribe: EventRegistration,
          receive: Event,
          renewSubscription: Lease,
        },
      ],
    ])
  );
  const client = new EventClient(cleanup.add(insecureChannel(server.addr), (c) => c.close()));
  return { e, client };
}

describe("SpaceClient", () => {
  it("write sends all options (string payload becomes bytes) and maps the tuple", async () => {
    const { s, client } = await startSpace();
    const t = await client.write({
      attrs: { a: "b" },
      payload: "data",
      ttlSeconds: 30,
      writtenBy: "writer",
      inputTupleId: "pred",
      txnId: "txn-1",
    });
    assert.deepEqual(s.write.attrs, { a: "b" });
    assert.equal(Buffer.from(s.write.payload).toString(), "data");
    assert.equal(Number(s.write.ttlSeconds), 30);
    assert.equal(s.write.writtenBy, "writer");
    assert.equal(s.write.inputTupleId, "pred");
    assert.equal(s.write.txnId, "txn-1");

    assert.equal(t.tupleId, "w");
    assert.equal(t.leaseId, "lease-w");
    assert.equal(t.writtenBy, "me");
    assert.equal(t.inputTupleId, "in");
    assert.equal(t.writtenAt?.getTime(), 50_000);
    assert.equal(t.payload.toString(), "pay");
    assert.deepEqual(t.attrs, { k: "v" });
  });

  it("write defaults optional fields", async () => {
    const { s, client } = await startSpace();
    await client.write({ attrs: { a: "b" }, ttlSeconds: 10 });
    assert.equal(Buffer.from(s.write.payload).length, 0);
    assert.equal(s.write.writtenBy, "");
    assert.equal(s.write.txnId, "");
  });

  it("read/take return null when there is no tuple", async () => {
    const { s, client } = await startSpace();
    s.hasMatch = false;
    assert.equal(await client.read({ template: { k: "v" } }), null);
    assert.equal(await client.take({ template: { k: "v" } }), null);
  });

  it("read/take pass wait, timeout and txn", async () => {
    const { s, client } = await startSpace();
    const r = await client.read({ template: { k: "v" }, wait: true, timeoutMs: 1500, txnId: "tx" });
    assert.equal(r?.tupleId, "r");
    assert.equal(s.read.wait, true);
    assert.equal(Number(s.read.timeoutMs), 1500);
    assert.equal(s.read.txnId, "tx");
    assert.deepEqual(s.read.template, { k: "v" });

    assert.equal((await client.take({ template: {}, wait: true, txnId: "tx2" }))?.tupleId, "t");
    assert.equal(s.take.wait, true);
    assert.equal(s.take.txnId, "tx2");

    await client.read({ template: {} });
    assert.equal(s.read.wait, false);
    assert.equal(s.read.txnId, "");
  });

  it("contents collects the stream", async () => {
    const { s, client } = await startSpace();
    const all = await client.contents({ k: "v" }, "tx");
    assert.deepEqual(all.map((t) => t.tupleId), ["c1", "c2"]);
    assert.equal(s.contents.txnId, "tx");
  });

  it("watch yields mapped events with handback and stops at stream end", async () => {
    const { s, client } = await startSpace();
    const got = [];
    for await (const evt of client.watch({
      template: { k: "v" },
      on: "expiration",
      ttlSeconds: 60,
      handback: Buffer.from("hb"),
    })) {
      got.push(evt);
    }
    assert.equal(s.notify.on, 1);
    assert.equal(Number(s.notify.ttlSeconds), 60);
    assert.equal(got.length, 2);
    assert.equal(got[0].type, "expiration");
    assert.equal(got[0].tuple?.tupleId, "n");
    assert.equal(got[0].handback.toString(), "hb");
    assert.equal(got[0].occurredAt?.getTime(), 9_000);
    assert.equal(got[1].type, "appearance");
    assert.equal(got[1].tuple, undefined);
  });

  it("notify defaults to appearance with a 60s ttl", async () => {
    const { s, client } = await startSpace();
    const stream = client.notify({ template: {} });
    for await (const _ of stream) {
      /* drain */
    }
    assert.equal(s.notify.on, 0);
    assert.equal(Number(s.notify.ttlSeconds), 60);
  });

  it("renewTuple and cancelTuple", async () => {
    const { s, client } = await startSpace();
    const lease = await client.renewTuple("tid", 12);
    assert.equal(lease.leaseId, "renewed-tid");
    assert.equal(lease.ttlSeconds, 12);
    await client.cancelTuple("tid");
    assert.equal(s.cancelled, "tid");
  });
});

describe("EventClient", () => {
  it("subscribe maps delivery mode and registration", async () => {
    const { e, client } = await startEvents();
    const reg = await client.subscribe({
      source: "src",
      template: { a: "b" },
      durable: true,
      ttlSeconds: 30,
      handback: Buffer.from("hb"),
    });
    assert.equal(e.subscribe.delivery, 0); // DURABLE
    assert.deepEqual(e.subscribe.template, { a: "b" });
    assert.equal(Buffer.from(e.subscribe.handback).toString(), "hb");
    assert.deepEqual(reg, { registrationId: "reg-1", source: "src", leaseId: "sub-lease", seqNum: 42 });

    await client.subscribe({ source: "src", ttlSeconds: 30 });
    assert.equal(e.subscribe.delivery, 1); // BEST_EFFORT
    assert.deepEqual(e.subscribe.template, {});
  });

  it("receive yields mapped events", async () => {
    const { client } = await startEvents();
    const got = [];
    for await (const evt of client.receive("reg-1")) got.push(evt);
    assert.equal(got.length, 2);
    assert.equal(got[0].eventId, "e1");
    assert.equal(got[0].eventType, "tick");
    assert.equal(got[0].seqNum, 3);
    assert.deepEqual(got[0].attrs, { a: "b" });
    assert.equal(got[0].payload.toString(), "p");
    assert.equal(got[0].handback.toString(), "hb");
    assert.equal(got[0].emittedAt?.getTime(), 5_000);
    assert.equal(got[1].emittedAt, undefined);
  });

  it("emit, renewSubscription and cancelSubscription", async () => {
    const { e, client } = await startEvents();
    await client.emit("src", "tick", { a: "b" }, Buffer.from("x"));
    assert.equal(e.emit.source, "src");
    assert.equal(e.emit.eventType, "tick");
    assert.equal(Buffer.from(e.emit.payload).toString(), "x");
    await client.emit("src", "tick");
    assert.deepEqual(e.emit.attrs, {});
    assert.equal(Buffer.from(e.emit.payload).length, 0);

    const lease = await client.renewSubscription("reg-1", 25);
    assert.equal(lease.leaseId, "reg-1");
    assert.equal(lease.ttlSeconds, 25);
    await client.cancelSubscription("reg-1");
    assert.equal(e.cancelled, "reg-1");
  });
});
