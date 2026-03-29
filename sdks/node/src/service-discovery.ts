import { DjinnClient } from "./djinn-client";
import type { Template } from "./registry-client";

interface CachedEntry {
  proxyId: string;
  localPort: number;
}

/**
 * Jini-inspired service discovery manager.
 *
 * Caches proxies by template. Repeated calls with the same template return
 * a cached address without any additional Djinn round-trips.
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

  private constructor(djinn: DjinnClient) {
    this.djinn = djinn;
  }

  /** Create a ServiceDiscovery manager backed by the given DjinnClient. */
  static watch(djinn: DjinnClient): ServiceDiscovery {
    return new ServiceDiscovery(djinn);
  }

  /**
   * Return a ready client for the first capability matching template.
   * Subsequent calls with the same template reuse a cached proxy port.
   *
   * @param factory  creates the client from a `"localhost:<port>"` address
   * @param template attribute map to match against the Registry
   */
  async get<T>(factory: (address: string) => T, template: Template): Promise<T> {
    const key = templateKey(template);

    let entry = this.cache.get(key);
    if (!entry) {
      const handle = await this.djinn.proxy().open(template);
      entry = { proxyId: handle.proxyId, localPort: handle.localPort };
      this.cache.set(key, entry);
    }

    return factory(`localhost:${entry.localPort}`);
  }

  /** Release all cached proxies. */
  async close(): Promise<void> {
    const entries = [...this.cache.values()];
    this.cache.clear();
    await Promise.all(entries.map(e => this.djinn.proxy().release(e.proxyId)));
  }
}

function templateKey(template: Template): string {
  return Object.keys(template)
    .sort()
    .map(k => `${k}=${template[k]}`)
    .join(",");
}
