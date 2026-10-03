import type { ClientReadableStream } from "@grpc/grpc-js";
import { DjinnClient } from "./djinn-client";
import type { Template, RegistryEventRecord } from "./registry-client";
import type { RegistryEvent } from "../gen/coordin8/registry";
import type { ProxyHandle } from "./proxy-client";

interface CachedEntry {
  proxyId: string;
  localPort: number;
  stale: boolean;
  handle: ProxyHandle;
}

/**
 * Jini-inspired service discovery manager.
 *
 * Caches proxies by template. Repeated calls with the same template return
 * a cached address without any additional Djinn round-trips. Watches the
 * Registry for changes and invalidates/refreshes cached connections.
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
  /**
   * Proxies replaced by a refresh while still alive. A caller may still hold
   * the old address (the proxy re-resolves per connection, so it keeps
   * working); they are released on close().
   */
  private readonly retired: string[] = [];
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
   * unless invalidated by a Registry Watch event.
   *
   * @param factory  creates the client from a `"localhost:<port>"` address
   * @param template attribute map to match against the Registry
   */
  async get<T>(factory: (address: string) => T, template: Template): Promise<T> {
    const key = templateKey(template);

    const entry = this.cache.get(key);
    // A lost lease (Djinn reclaimed the proxy) counts as stale: reopen.
    if (entry && !entry.stale && !entry.handle.isLost()) {
      return factory(`localhost:${entry.localPort}`);
    }

    const refreshed = await this.refresh(key, template);
    return factory(`localhost:${refreshed.localPort}`);
  }

  private async refresh(key: string, template: Template): Promise<CachedEntry> {
    // Replace the stale entry. A caller may still hold its address, so only
    // release it now if the proxy lease is gone (it is dead anyway);
    // otherwise retire it until close().
    const old = this.cache.get(key);
    if (old) {
      if (old.handle.isLost()) {
        await this.djinn.proxy().release(old.proxyId).catch(() => {});
      } else {
        this.retired.push(old.proxyId);
      }
    }

    const handle = await this.djinn.proxy().open(template);
    const entry: CachedEntry = {
      proxyId: handle.proxyId,
      localPort: handle.localPort,
      stale: false,
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
      (evt: RegistryEventRecord) => {
        if (this.closed) return;
        const entry = this.cache.get(key);
        if (!entry) return;

        switch (evt.type) {
          case "expired":
            entry.stale = true;
            break;
          case "registered":
            if (entry.stale) {
              this.refresh(key, template).catch(() => {});
            }
            break;
          case "modified":
            // Nothing to do: Proxy re-resolves the upstream on every new TCP
            // connection, so the existing proxy port already reaches the
            // modified service. Reopening would risk breaking clients callers hold.
            break;
        }
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
    const ids = [...this.cache.values()].map(e => e.proxyId).concat(this.retired);
    this.cache.clear();
    this.retired.length = 0;
    await Promise.all(ids.map(id => this.djinn.proxy().release(id)));
  }
}

function templateKey(template: Template): string {
  return Object.keys(template)
    .sort()
    .map(k => `${k}=${template[k]}`)
    .join(",");
}
