import * as grpc from "@grpc/grpc-js";
import { LeaseClient } from "./lease-client";
import { RegistryClient } from "./registry-client";
import { ProxyClient } from "./proxy-client";
import { SpaceClient } from "./space-client";
import { EventClient } from "./event-client";
import { interceptorsFor } from "./auth";

/**
 * Options for DjinnClient.connect(). Use these to pin a specific service's
 * address instead of looking it up through Registry.
 */
export interface ConnectOptions {
  /** Pin Proxy's address instead of looking it up through Registry. */
  proxyAddr?: string;
  /** Pin Space's address instead of looking it up through Registry. */
  spaceAddr?: string;
  /** Pin EventMgr's address instead of looking it up through Registry. */
  eventAddr?: string;
  /**
   * Bearer token attached to every call on every connection — required
   * against a Djinn with COORDIN8_JWT_SECRET set, harmless (ignored)
   * against one that doesn't have auth enabled. Mint one with
   * `coordin8 auth mint-token`; see .claude/plans/grpc-security/PRD.md.
   */
  token?: string;
}

/**
 * DjinnClient is the entry point for all Djinn interactions.
 *
 * There is no single "LeaseMgr" to connect to — leasing is distributed.
 * Registry, Space, and EventMgr each grant their own leases and mount
 * LeaseService on their own connection; renew/cancel a lease by dialing
 * whichever one granted it (its address is carried on the Lease itself, in
 * `grantorHost`/`grantorPort`) via `dialLease()`. See
 * .claude/plans/distributed-leasing/PRD.md.
 */
export class DjinnClient {
  private readonly registryChannel: grpc.Channel;
  private readonly proxyChannel: grpc.Channel;
  private readonly spaceChannel: grpc.Channel;
  private readonly eventChannel: grpc.Channel;
  private readonly interceptors: grpc.Interceptor[];
  private readonly _registry: RegistryClient;
  private readonly _proxy: ProxyClient;
  private readonly _space: SpaceClient;
  private readonly _event: EventClient;

  private constructor(
    registryChannel: grpc.Channel,
    proxyChannel: grpc.Channel,
    spaceChannel: grpc.Channel,
    eventChannel: grpc.Channel,
    interceptors: grpc.Interceptor[]
  ) {
    this.registryChannel = registryChannel;
    this.proxyChannel = proxyChannel;
    this.spaceChannel = spaceChannel;
    this.eventChannel = eventChannel;
    this.interceptors = interceptors;
    this._registry = new RegistryClient(registryChannel, interceptors);
    this._proxy = new ProxyClient(proxyChannel, interceptors);
    this._space = new SpaceClient(spaceChannel, interceptors);
    this._event = new EventClient(eventChannel, interceptors);
  }

  /**
   * Dials Registry directly at registryAddr — the one address a caller
   * needs to know in advance — then looks up Proxy, Space, and EventMgr
   * through it, the same way any application service is discovered via
   * ServiceDiscovery. Works identically against a bundled monolith or a
   * fully split, multi-host deployment: Registry just returns whatever
   * address each service actually registered.
   *
   * Use opts.proxyAddr / opts.spaceAddr / opts.eventAddr to pin a specific
   * service's address instead of looking it up.
   */
  static async connect(registryAddr: string, opts: ConnectOptions = {}): Promise<DjinnClient> {
    const creds = grpc.credentials.createInsecure();
    const interceptors = interceptorsFor(opts.token);
    const registryChannel = new grpc.Channel(registryAddr, creds, {});
    const registry = new RegistryClient(registryChannel, interceptors);

    let proxyAddr: string;
    let spaceAddr: string;
    let eventAddr: string;
    try {
      [proxyAddr, spaceAddr, eventAddr] = await Promise.all([
        resolveAddr(registry, opts.proxyAddr, "Proxy"),
        resolveAddr(registry, opts.spaceAddr, "Space"),
        resolveAddr(registry, opts.eventAddr, "EventMgr"),
      ]);
    } catch (err) {
      registryChannel.close();
      throw err;
    }

    const proxyChannel = new grpc.Channel(proxyAddr, creds, {});
    const spaceChannel = new grpc.Channel(spaceAddr, creds, {});
    const eventChannel = new grpc.Channel(eventAddr, creds, {});

    return new DjinnClient(registryChannel, proxyChannel, spaceChannel, eventChannel, interceptors);
  }

  registry(): RegistryClient { return this._registry; }
  proxy():    ProxyClient    { return this._proxy; }
  space():    SpaceClient    { return this._space; }
  events():   EventClient    { return this._event; }

  /**
   * Returns a LeaseClient for renewing/cancelling leases that Registry
   * itself granted — every register() call returns one. LeaseService is
   * mounted on Registry's own connection (no separate dial needed),
   * matching how Registry embeds its own LeaseManager.
   */
  registryLeases(): LeaseClient {
    return LeaseClient.fromChannel(this.registryChannel, this.interceptors);
  }

  close(): void {
    this.registryChannel.close();
    this.proxyChannel.close();
    this.spaceChannel.close();
    this.eventChannel.close();
  }
}

/**
 * Looks up interfaceName in Registry and returns its "host:port" transport
 * address, unless pinned overrides the lookup.
 */
async function resolveAddr(
  registry: RegistryClient,
  pinned: string | undefined,
  interfaceName: string
): Promise<string> {
  if (pinned) return pinned;

  const record = await registry.lookup({ interface: interfaceName });
  if (!record) {
    throw new Error(`look up ${interfaceName}: not found in registry`);
  }
  const host = record.transport?.config?.host;
  const port = record.transport?.config?.port;
  if (!host || !port) {
    throw new Error(`look up ${interfaceName}: missing host/port in transport config`);
  }
  return `${host}:${port}`;
}
