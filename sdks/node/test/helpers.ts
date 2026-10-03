import * as grpc from "@grpc/grpc-js";

/** A loosely-typed handler map; fakes only implement the RPCs they need. */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export type Impl = Record<string, (call: any, cb: any) => void>;

/**
 * Maps an RPC name to its generated response type. The ts-proto encoder
 * needs every field present, so fakes may return partial messages and we
 * complete them with `fromPartial` before they hit the wire.
 */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export type Responses = Record<string, { fromPartial(x: any): any }>;

function withDefaults(impl: Impl, responses: Responses): Impl {
  const out: Impl = {};
  for (const [name, handler] of Object.entries(impl)) {
    const codec = responses[name];
    if (!codec) {
      out[name] = handler;
      continue;
    }
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    out[name] = (call: any, cb: any) => {
      if (typeof cb === "function") {
        // unary
        return handler(call, (err: unknown, v: unknown) => cb(err, v == null ? v : codec.fromPartial(v)));
      }
      // server-streaming: complete each written message
      const write = call.write.bind(call);
      call.write = (v: unknown) => write(codec.fromPartial(v));
      return handler(call, cb);
    };
  }
  return out;
}

export interface TestServer {
  /** "127.0.0.1:<port>" */
  addr: string;
  port: number;
  stop(): void;
}

/**
 * Starts a real @grpc/grpc-js server on an ephemeral localhost port, serving
 * the given generated service definitions with the given fake implementations.
 */
export async function serve(
  services: Array<[grpc.ServiceDefinition, Impl, Responses?]>
): Promise<TestServer> {
  const server = new grpc.Server();
  for (const [def, impl, responses] of services) {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    server.addService(def, withDefaults(impl, responses ?? {}) as any);
  }
  const port = await new Promise<number>((resolve, reject) => {
    server.bindAsync("127.0.0.1:0", grpc.ServerCredentials.createInsecure(), (err, p) =>
      err ? reject(err) : resolve(p)
    );
  });
  return { addr: `127.0.0.1:${port}`, port, stop: () => server.forceShutdown() };
}

export function insecureChannel(addr: string): grpc.Channel {
  return new grpc.Channel(addr, grpc.credentials.createInsecure(), {});
}

export function grpcError(code: grpc.status, details = "test error"): grpc.ServiceError {
  return Object.assign(new Error(details), {
    code,
    details,
    metadata: new grpc.Metadata(),
  }) as grpc.ServiceError;
}

/** Polls until cond() is truthy or timeout; returns the final value. */
export async function waitFor(cond: () => boolean, timeoutMs = 5000, stepMs = 20): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (cond()) return true;
    await sleep(stepMs);
  }
  return cond();
}

export function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}

/** Collects teardown functions so each test can clean up everything it started. */
export class Cleanup {
  private readonly fns: Array<() => void | Promise<void>> = [];
  add<T>(thing: T, fn: (t: T) => void | Promise<void>): T {
    this.fns.push(() => fn(thing));
    return thing;
  }
  server(s: TestServer): TestServer {
    return this.add(s, (x) => x.stop());
  }
  async run(): Promise<void> {
    for (const fn of this.fns.reverse()) await fn();
    this.fns.length = 0;
  }
}
