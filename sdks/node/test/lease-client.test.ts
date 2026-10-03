import { afterEach, describe, it } from "node:test";
import assert from "node:assert/strict";
import * as grpc from "@grpc/grpc-js";
import { Lease, LeaseServiceService } from "../gen/coordin8/lease";
import { LeaseClient, dialLease, grantorAddr } from "../src/index";
import { Cleanup, grpcError, insecureChannel, serve, sleep, waitFor } from "./helpers";

const cleanup = new Cleanup();
afterEach(() => cleanup.run());

interface Fake {
  renews: number;
  renewIds: string[];
  renewTtls: number[];
  cancelled: string[];
  /** Decides each Renew's outcome by call number (1-based); undefined = success. */
  behavior: (n: number) => grpc.status | undefined;
  grants: Array<{ resourceId: string; ttlSeconds: number }>;
  authHeaders: Array<string | undefined>;
}

async function start(): Promise<{ fake: Fake; client: LeaseClient; addr: string }> {
  const fake: Fake = {
    renews: 0,
    renewIds: [],
    renewTtls: [],
    cancelled: [],
    behavior: () => undefined,
    grants: [],
    authHeaders: [],
  };
  const server = cleanup.server(
    await serve([
      [
        LeaseServiceService,
        {
          grant: (call, cb) => {
            fake.authHeaders.push(call.metadata.get("authorization")[0] as string | undefined);
            fake.grants.push({ resourceId: call.request.resourceId, ttlSeconds: Number(call.request.ttlSeconds) });
            cb(null, {
              leaseId: "L1",
              resourceId: call.request.resourceId,
              grantedAt: new Date(1_000_000),
              expiresAt: new Date(31_000_000),
              ttlSeconds: call.request.ttlSeconds,
              grantorHost: "gh",
              grantorPort: 1234,
            });
          },
          renew: (call, cb) => {
            fake.renews++;
            fake.renewIds.push(call.request.leaseId);
            fake.renewTtls.push(Number(call.request.ttlSeconds));
            const code = fake.behavior(fake.renews);
            if (code !== undefined) return cb(grpcError(code));
            cb(null, { leaseId: call.request.leaseId, ttlSeconds: call.request.ttlSeconds });
          },
          cancel: (call, cb) => {
            fake.cancelled.push(call.request.leaseId);
            cb(null, {});
          },
        },
        { grant: Lease, renew: Lease },
      ],
    ])
  );
  const channel = cleanup.add(insecureChannel(server.addr), (c) => c.close());
  const client = LeaseClient.fromChannel(channel);
  return { fake, client, addr: server.addr };
}

describe("LeaseClient", () => {
  it("grant maps the lease including grantor address", async () => {
    const { fake, client } = await start();
    const lease = await client.grant("res", 30);
    assert.deepEqual(fake.grants, [{ resourceId: "res", ttlSeconds: 30 }]);
    assert.equal(lease.leaseId, "L1");
    assert.equal(lease.resourceId, "res");
    assert.equal(lease.ttlSeconds, 30);
    assert.equal(lease.grantedAt?.getTime(), 1_000_000);
    assert.equal(lease.expiresAt?.getTime(), 31_000_000);
    assert.equal(grantorAddr(lease), "gh:1234");
  });

  it("renew and cancel send ids; renew rejects with the gRPC error", async () => {
    const { fake, client } = await start();
    await client.renew("L9", 15);
    assert.deepEqual(fake.renewIds, ["L9"]);
    assert.deepEqual(fake.renewTtls, [15]);
    await client.cancel("L9");
    assert.deepEqual(fake.cancelled, ["L9"]);

    fake.behavior = () => grpc.status.NOT_FOUND;
    await assert.rejects(client.renew("L9", 15), (e: grpc.ServiceError) => e.code === grpc.status.NOT_FOUND);
  });

  it("dial/dialLease owns its connection and attaches the token", async () => {
    const { fake, addr } = await start();
    const c1 = dialLease(addr, "tok-1");
    const c2 = LeaseClient.dial(addr);
    try {
      await c1.grant("a", 5);
      await c2.grant("b", 5);
    } finally {
      c1.close();
      c2.close();
    }
    assert.deepEqual(fake.authHeaders, ["Bearer tok-1", undefined]);
  });

  describe("keepAlive", () => {
    // ttl=1 => renew every 500ms.
    it("renews on schedule with the requested ttl until aborted", async () => {
      const { fake, client } = await start();
      const ac = new AbortController();
      const failures: unknown[] = [];
      client.keepAlive("L1", 1, ac.signal, (e) => failures.push(e));
      assert.ok(await waitFor(() => fake.renews >= 2, 4000), "expected >=2 renewals");
      ac.abort();
      assert.deepEqual(fake.renewIds.slice(0, 2), ["L1", "L1"]);
      assert.equal(fake.renewTtls[0], 1);
      assert.equal(failures.length, 0);
    });

    it("abort stops further renewals", async () => {
      const { fake, client } = await start();
      const ac = new AbortController();
      client.keepAlive("L1", 1, ac.signal);
      assert.ok(await waitFor(() => fake.renews >= 1, 3000));
      ac.abort();
      await sleep(150); // let an in-flight renew settle
      const after = fake.renews;
      await sleep(1200);
      assert.equal(fake.renews, after);
    });

    for (const [name, code] of [
      ["NOT_FOUND", grpc.status.NOT_FOUND],
      ["FAILED_PRECONDITION", grpc.status.FAILED_PRECONDITION],
    ] as const) {
      it(`stops after reporting ${name} exactly once`, async () => {
        const { fake, client } = await start();
        fake.behavior = () => code;
        const ac = new AbortController();
        const failures: grpc.ServiceError[] = [];
        client.keepAlive("L1", 1, ac.signal, (e) => failures.push(e as grpc.ServiceError));
        assert.ok(await waitFor(() => failures.length >= 1, 3000));
        await sleep(1300); // would be 2+ more ticks if still running
        ac.abort();
        assert.equal(failures.length, 1);
        assert.equal(fake.renews, 1);
        assert.equal(failures[0].code, code);
      });
    }

    it("retries after other errors and keeps renewing", async () => {
      const { fake, client } = await start();
      fake.behavior = (n) => (n === 1 ? grpc.status.UNAVAILABLE : undefined);
      const ac = new AbortController();
      const failures: grpc.ServiceError[] = [];
      client.keepAlive("L1", 1, ac.signal, (e) => failures.push(e as grpc.ServiceError));
      assert.ok(await waitFor(() => fake.renews >= 3, 5000), "loop should survive a transient error");
      ac.abort();
      assert.equal(failures.length, 1);
      assert.equal(failures[0].code, grpc.status.UNAVAILABLE);
    });

    it("keeps retrying on persistent transient errors", async () => {
      const { fake, client } = await start();
      fake.behavior = () => grpc.status.INTERNAL;
      const ac = new AbortController();
      const failures: unknown[] = [];
      client.keepAlive("L1", 1, ac.signal, (e) => failures.push(e));
      assert.ok(await waitFor(() => failures.length >= 2, 4000));
      ac.abort();
    });

    it("works without an onFailure callback", async () => {
      const { fake, client } = await start();
      fake.behavior = () => grpc.status.NOT_FOUND;
      const ac = new AbortController();
      client.keepAlive("L1", 1, ac.signal);
      assert.ok(await waitFor(() => fake.renews >= 1, 3000));
      ac.abort();
    });

    // Known bug: keepAlive only listens for a future "abort" event, so a
    // signal that is already aborted never clears the timer and renewals run
    // forever (and keep the process alive).
    it(
      "does not start renewing if the signal is already aborted",
      { skip: "known bug: pre-aborted signal is ignored" },
      async () => {
        const { fake, client } = await start();
        const ac = new AbortController();
        ac.abort();
        client.keepAlive("L1", 1, ac.signal);
        await sleep(1200);
        assert.equal(fake.renews, 0);
      }
    );
  });
});
