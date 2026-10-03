import { afterEach, describe, it } from "node:test";
import assert from "node:assert/strict";
import * as grpc from "@grpc/grpc-js";
import { Lease, LeaseServiceService } from "../gen/coordin8/lease";
import { interceptorsFor } from "../src/auth";
import { LeaseClient } from "../src/index";
import { Cleanup, insecureChannel, serve } from "./helpers";

const cleanup = new Cleanup();
afterEach(() => cleanup.run());

async function captureServer() {
  const seen: string[][] = [];
  const server = cleanup.server(
    await serve([
      [
        LeaseServiceService,
        {
          grant: (call, cb) => {
            seen.push(call.metadata.get("authorization").map(String));
            cb(null, { leaseId: "L" });
          },
        },
        { grant: Lease },
      ],
    ])
  );
  return { seen, server };
}

describe("bearer token interceptor", () => {
  it("interceptorsFor returns nothing without a token", () => {
    assert.deepEqual(interceptorsFor(undefined), []);
    assert.deepEqual(interceptorsFor(""), []);
    assert.equal(interceptorsFor("t").length, 1);
  });

  it("attaches 'authorization: Bearer <token>' to every call", async () => {
    const { seen, server } = await captureServer();
    const ch = cleanup.add(insecureChannel(server.addr), (c) => c.close());
    const client = LeaseClient.fromChannel(ch, interceptorsFor("tok123"));
    await client.grant("a", 1);
    await client.grant("b", 1);
    assert.deepEqual(seen, [["Bearer tok123"], ["Bearer tok123"]]);
  });

  it("sends no authorization header when there is no token", async () => {
    const { seen, server } = await captureServer();
    const ch = cleanup.add(insecureChannel(server.addr), (c) => c.close());
    await LeaseClient.fromChannel(ch).grant("a", 1);
    assert.deepEqual(seen, [[]]);
  });

  it("insecure credentials cannot compose call credentials (why this is an interceptor)", () => {
    assert.throws(() =>
      grpc.credentials
        .createInsecure()
        .compose(grpc.credentials.createFromMetadataGenerator((_p, cb) => cb(null, new grpc.Metadata())))
    );
  });
});
