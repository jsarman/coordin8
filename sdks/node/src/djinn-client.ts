import * as grpc from "@grpc/grpc-js";
import { LeaseClient } from "./lease-client";
import { RegistryClient } from "./registry-client";
import { ProxyClient } from "./proxy-client";
import { SpaceClient } from "./space-client";

const LEASE_PORT    = 9001;
const REGISTRY_PORT = 9002;
const PROXY_PORT    = 9003;
const SPACE_PORT    = 9006;

export class DjinnClient {
  private readonly leaseChannel:    grpc.Channel;
  private readonly registryChannel: grpc.Channel;
  private readonly proxyChannel:    grpc.Channel;
  private readonly spaceChannel:    grpc.Channel;
  private readonly _lease:    LeaseClient;
  private readonly _registry: RegistryClient;
  private readonly _proxy:    ProxyClient;
  private readonly _space:    SpaceClient;

  private constructor(host: string) {
    const creds = grpc.credentials.createInsecure();
    this.leaseChannel    = new grpc.Channel(`${host}:${LEASE_PORT}`,    creds, {});
    this.registryChannel = new grpc.Channel(`${host}:${REGISTRY_PORT}`, creds, {});
    this.proxyChannel    = new grpc.Channel(`${host}:${PROXY_PORT}`,    creds, {});
    this.spaceChannel    = new grpc.Channel(`${host}:${SPACE_PORT}`,    creds, {});
    this._lease    = new LeaseClient(this.leaseChannel);
    this._registry = new RegistryClient(this.registryChannel);
    this._proxy    = new ProxyClient(this.proxyChannel);
    this._space    = new SpaceClient(this.spaceChannel);
  }

  static connect(host: string = "localhost"): DjinnClient {
    return new DjinnClient(host);
  }

  lease():    LeaseClient    { return this._lease; }
  registry(): RegistryClient { return this._registry; }
  proxy():    ProxyClient    { return this._proxy; }
  space():    SpaceClient    { return this._space; }

  close(): void {
    this.leaseChannel.close();
    this.registryChannel.close();
    this.proxyChannel.close();
    this.spaceChannel.close();
  }
}
