import * as grpc from "@grpc/grpc-js";
import { ProxyServiceClient } from "../gen/coordin8/proxy";
import type { Template } from "./registry-client";
import { LeaseClient, type Lease } from "./lease-client";

/** Lease TTL (seconds) requested by `open()`; renewed in the background until `release()`. */
export const DEFAULT_PROXY_TTL_SECONDS = 30;

export interface ProxyHandle {
  proxyId: string;
  localPort: number;
  /** The proxy's lease; its grantor is the Proxy itself. */
  lease?: Lease;
  /**
   * True once keep-alive learned the Djinn reclaimed the proxy
   * (NOT_FOUND / FAILED_PRECONDITION on renew) — it must be reopened.
   */
  isLost(): boolean;
}

export class ProxyClient {
  private readonly stub: ProxyServiceClient;
  /** LeaseService is mounted on Proxy's own channel (Proxy is a Landlord). */
  private readonly leases: LeaseClient;
  private readonly keepAlives = new Map<string, AbortController>();

  constructor(channel: grpc.Channel, interceptors: grpc.Interceptor[] = []) {
    this.stub = new ProxyServiceClient(
      "passthrough:///djinn",
      grpc.credentials.createInsecure(),
      { channelOverride: channel, interceptors }
    );
    this.leases = LeaseClient.fromChannel(channel, interceptors);
  }

  /**
   * Ask the Djinn to open a local TCP forwarding port for the given template.
   * The proxy is leased (`ttlSeconds`, default 30s; 0 = server's preferred
   * TTL) and the SDK renews it in the background until `release()`.
   */
  open(template: Template, ttlSeconds: number = DEFAULT_PROXY_TTL_SECONDS): Promise<ProxyHandle> {
    return new Promise((resolve, reject) => {
      this.stub.open({ template, ttlSeconds }, (err, res) => {
        if (err || !res) return reject(err);
        let lost = false;
        let lease: Lease | undefined;
        if (res.lease) {
          const l = res.lease;
          lease = {
            leaseId: l.leaseId,
            resourceId: l.resourceId,
            grantedAt: l.grantedAt ?? undefined,
            expiresAt: l.expiresAt ?? undefined,
            ttlSeconds: l.ttlSeconds,
            grantorHost: l.grantorHost,
            grantorPort: l.grantorPort,
          };
          const ac = new AbortController();
          this.keepAlives.set(res.proxyId, ac);
          // Renew with the TTL as granted (the server may have negotiated it).
          this.leases.keepAlive(l.leaseId, l.ttlSeconds > 0 ? l.ttlSeconds : ttlSeconds, ac.signal, (e) => {
            const code = (e as grpc.ServiceError)?.code;
            if (code === grpc.status.NOT_FOUND || code === grpc.status.FAILED_PRECONDITION) {
              lost = true;
            }
          });
        }
        resolve({ proxyId: res.proxyId, localPort: res.localPort, lease, isLost: () => lost });
      });
    });
  }

  /** Stop lease renewal and release a proxy on the Djinn. */
  release(proxyId: string): Promise<void> {
    this.keepAlives.get(proxyId)?.abort();
    this.keepAlives.delete(proxyId);
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
