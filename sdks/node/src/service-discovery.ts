import type { ClientReadableStream } from "@grpc/grpc-js";
import { DjinnClient } from "./djinn-client";
import type { Template, RegistryEventRecord } from "./registry-client";
import type { RegistryEvent } from "../gen/coordin8/registry";
import type { ProxyHandle } from "./proxy-client";

interface CachedEntry {
  proxyId: string;
  localPort: number;
  handle: ProxyHandle;
}

/**
 * Jini-inspired service discovery manager.
 *
 * Keeps one leased proxy per template. Repeated calls with the same template
 * return a cached address without any additional Djinn round-trips. Proxy
 * re-resolves the upstream on every new TCP connection, so a client held on
 * the address survives the service expiring, restarting or moving: while the
 * service is absent, RPCs fail at connect time and succeed again once it
 * re-registers, with no client action. The cache entry is replaced only if
 * the proxy's own lease is lost.
 *
 * ```typescript
 * const discovery = ServiceDiscovery.watch(djinn);
 *
 * const greeter = await discovery.get(
 *   addr => new GreeterServiceClient(addr, grpc.credentials.createInsecure()),
 *   { interface: "Greeter" }
 * );
 *
 * await discovery.close();
 * ```
 */
export class ServiceDiscovery {
  private readonly djinn: DjinnClient;
  private readonly cache = new Map<string, CachedEntry>();
  private readonly watches = new Map<string, ClientReadableStream<RegistryEvent>>();
  private closed = false;

  private constructor(djinn: DjinnClient) {
    this.djinn = djinn;
  }

  /** Create a ServiceDiscovery manager backed by the given DjinnClient. */
  static watch(djinn: DjinnClient): ServiceDiscovery {
    return new ServiceDiscovery(djinn);
  }

  /**
   * Return a ready client for the first capability matching template.
   * Subsequent calls with the same template reuse a cached proxy port
   * unless the proxy's own lease was lost.
   *
   * @param factory  creates the client from a `"localhost:<port>"` address
   * @param template attribute map to match against the Registry
   */
  async get<T>(factory: (address: string) => T, template: Template): Promise<T> {
    const key = templateKey(template);

    const entry = this.cache.get(key);
    // Only a lost proxy lease (Djinn reclaimed the proxy) forces a reopen.
    if (entry && !entry.handle.isLost()) {
      return factory(`localhost:${entry.localPort}`);
    }

    const refreshed = await this.refresh(key, template);
    return factory(`localhost:${refreshed.localPort}`);
  }

  private async refresh(key: string, template: Template): Promise<CachedEntry> {
    // The entry is only ever replaced because its proxy lease was lost, so
    // the proxy is already dead; release it best-effort.
    const old = this.cache.get(key);
    if (old) {
      await this.djinn.proxy().release(old.proxyId).catch(() => {});
    }

    const handle = await this.djinn.proxy().open(template);
    const entry: CachedEntry = {
      proxyId: handle.proxyId,
      localPort: handle.localPort,
      handle,
    };
    this.cache.set(key, entry);

    // Start watching if not already
    if (!this.watches.has(key)) {
      this.startWatch(key, template);
    }

    return entry;
  }

  private startWatch(key: string, template: Template): void {
    const stream = this.djinn.registry().watch(
      template,
      (_evt: RegistryEventRecord) => {
        // "expired" / "registered" / "modified": nothing to do. Proxy
        // re-resolves the upstream on every new TCP connection, so the
        // existing proxy stays valid while the service restarts or moves.
        // Only a lost proxy lease replaces an entry (see get()).
      },
      (_err: Error) => {
        if (this.closed) return;
        this.watches.delete(key);
        // Reconnect after a brief pause
        setTimeout(() => {
          if (!this.closed && this.cache.has(key)) {
            this.startWatch(key, template);
          }
        }, 2000);
      }
    );
    this.watches.set(key, stream);
  }

  /** Release all cached proxies and stop watches. */
  async close(): Promise<void> {
    this.closed = true;
    for (const stream of this.watches.values()) {
      stream.cancel();
    }
    this.watches.clear();
    const ids = [...this.cache.values()].map(e => e.proxyId);
    this.cache.clear();
    await Promise.all(ids.map(id => this.djinn.proxy().release(id)));
  }
}

function templateKey(template: Template): string {
  return Object.keys(template)
    .sort()
    .map(k => `${k}=${template[k]}`)
    .join(",");
}
