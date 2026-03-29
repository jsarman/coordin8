import * as grpc from "@grpc/grpc-js";
import { ProxyServiceClient } from "../gen/coordin8/proxy";
import type { Template } from "./registry-client";

export interface ProxyHandle {
  proxyId: string;
  localPort: number;
}

export class ProxyClient {
  private readonly stub: ProxyServiceClient;

  constructor(channel: grpc.Channel) {
    this.stub = new ProxyServiceClient(
      "passthrough:///djinn",
      grpc.credentials.createInsecure(),
      { channelOverride: channel }
    );
  }

  /** Ask the Djinn to open a local TCP forwarding port for the given template. */
  open(template: Template): Promise<ProxyHandle> {
    return new Promise((resolve, reject) => {
      this.stub.open({ template }, (err, res) => {
        if (err || !res) return reject(err);
        resolve({ proxyId: res.proxyId, localPort: res.localPort });
      });
    });
  }

  /** Release a proxy on the Djinn. */
  release(proxyId: string): Promise<void> {
    return new Promise((resolve, reject) => {
      this.stub.release({ proxyId }, (err) => {
        if (err) return reject(err);
        resolve();
      });
    });
  }

  /**
   * One-liner: open a proxy, resolve the local address, and pass it to a
   * factory function that creates the client.
   *
   * The factory receives `"localhost:<port>"` — use it to create any gRPC
   * client with your own credentials and options.
   *
   * ```typescript
   * const greeter = await djinn.proxy().proxyClient(
   *   addr => new GreeterServiceClient(addr, grpc.credentials.createInsecure()),
   *   { interface: "Greeter" }
   * );
   * ```
   */
  async proxyClient<T>(
    factory: (address: string) => T,
    template: Template
  ): Promise<T> {
    const handle = await this.open(template);
    return factory(`localhost:${handle.localPort}`);
  }
}
