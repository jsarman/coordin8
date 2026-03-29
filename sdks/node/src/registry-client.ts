import * as grpc from "@grpc/grpc-js";
import { RegistryServiceClient } from "../gen/coordin8/registry";
import type { Capability } from "../gen/coordin8/registry";

export interface TransportDescriptor {
  type: string;
  config: Record<string, string>;
}

export interface CapabilityRecord {
  capabilityId: string;
  interfaceName: string;
  attrs: Record<string, string>;
  transport?: TransportDescriptor;
}

export type Template = Record<string, string>;

function toRecord(c: Capability): CapabilityRecord {
  return {
    capabilityId: c.capabilityId,
    interfaceName: c.interface,
    attrs: c.attrs,
    transport: c.transport
      ? { type: c.transport.type, config: c.transport.config }
      : undefined,
  };
}

export class RegistryClient {
  private readonly stub: RegistryServiceClient;

  constructor(channel: grpc.Channel) {
    this.stub = new RegistryServiceClient(
      "passthrough:///djinn",
      grpc.credentials.createInsecure(),
      { channelOverride: channel }
    );
  }

  register(
    interfaceName: string,
    attrs: Record<string, string>,
    transport: TransportDescriptor,
    ttlSeconds: number
  ): Promise<string> {
    return new Promise((resolve, reject) => {
      this.stub.register(
        { interface: interfaceName, attrs, transport, ttlSeconds },
        (err, res) => {
          if (err || !res) return reject(err);
          resolve(res.leaseId);
        }
      );
    });
  }

  lookup(template: Template): Promise<CapabilityRecord | null> {
    return new Promise((resolve, reject) => {
      this.stub.lookup({ template }, (err, res) => {
        if (err) {
          if (err.code === grpc.status.NOT_FOUND) return resolve(null);
          return reject(err);
        }
        resolve(res ? toRecord(res) : null);
      });
    });
  }

  lookupAll(template: Template): Promise<CapabilityRecord[]> {
    return new Promise((resolve, reject) => {
      const results: CapabilityRecord[] = [];
      const stream = this.stub.lookupAll({ template });
      stream.on("data", (cap: Capability) => results.push(toRecord(cap)));
      stream.on("end", () => resolve(results));
      stream.on("error", reject);
    });
  }
}
