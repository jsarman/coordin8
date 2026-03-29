import * as grpc from "@grpc/grpc-js";
import { LeaseClient } from "./lease-client";
import { RegistryClient } from "./registry-client";

const LEASE_PORT    = 9001;
const REGISTRY_PORT = 9002;

export class DjinnClient {
  private readonly leaseChannel:    grpc.Channel;
  private readonly registryChannel: grpc.Channel;
  private readonly _lease:    LeaseClient;
  private readonly _registry: RegistryClient;

  private constructor(host: string) {
    const creds = grpc.credentials.createInsecure();
    this.leaseChannel    = new grpc.Channel(`${host}:${LEASE_PORT}`,    creds, {});
    this.registryChannel = new grpc.Channel(`${host}:${REGISTRY_PORT}`, creds, {});
    this._lease    = new LeaseClient(this.leaseChannel);
    this._registry = new RegistryClient(this.registryChannel);
  }

  static connect(host: string = "localhost"): DjinnClient {
    return new DjinnClient(host);
  }

  lease():    LeaseClient    { return this._lease; }
  registry(): RegistryClient { return this._registry; }

  close(): void {
    this.leaseChannel.close();
    this.registryChannel.close();
  }
}
