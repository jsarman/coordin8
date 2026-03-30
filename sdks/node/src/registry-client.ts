import * as grpc from "@grpc/grpc-js";
import { RegistryServiceClient } from "../gen/coordin8/registry";
import type {
  Capability,
  RegistryEvent,
  RegistryEvent_EventType,
} from "../gen/coordin8/registry";

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

export interface RegisterResult {
  capabilityId: string;
  leaseId: string;
  grantedTtlSeconds: number;
}

export interface RegistryEventRecord {
  type: "registered" | "expired" | "modified";
  capability?: CapabilityRecord;
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

function eventTypeToString(t: RegistryEvent_EventType): "registered" | "expired" | "modified" {
  switch (t) {
    case 1: return "expired";
    case 2: return "modified";
    default: return "registered";
  }
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

  /**
   * Register a service capability. Returns capability ID and lease info.
   * Pass capabilityId to update an existing registration in-place.
   */
  register(
    interfaceName: string,
    attrs: Record<string, string>,
    transport: TransportDescriptor | undefined,
    ttlSeconds: number,
    capabilityId?: string
  ): Promise<RegisterResult> {
    return new Promise((resolve, reject) => {
      this.stub.register(
        {
          interface: interfaceName,
          attrs,
          transport,
          ttlSeconds,
          capabilityId: capabilityId ?? "",
        },
        (err, res) => {
          if (err || !res) return reject(err);
          resolve({
            capabilityId: res.capabilityId,
            leaseId: res.lease?.leaseId ?? "",
            grantedTtlSeconds: res.lease?.ttlSeconds ?? 0,
          });
        }
      );
    });
  }

  /**
   * Modify attributes on an existing registration without re-registering.
   */
  modifyAttrs(
    capabilityId: string,
    addAttrs?: Record<string, string>,
    removeAttrs?: string[]
  ): Promise<CapabilityRecord> {
    return new Promise((resolve, reject) => {
      this.stub.modifyAttrs(
        {
          capabilityId,
          addAttrs: addAttrs ?? {},
          removeAttrs: removeAttrs ?? [],
        },
        (err, res) => {
          if (err || !res) return reject(err);
          resolve(toRecord(res));
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

  /**
   * Watch for registry events matching the template.
   * Returns a stream that emits RegistryEventRecord objects.
   * Call stream.cancel() to stop watching.
   */
  watch(
    template: Template,
    onEvent: (evt: RegistryEventRecord) => void,
    onError?: (err: Error) => void
  ): grpc.ClientReadableStream<RegistryEvent> {
    const stream = this.stub.watch({ template });
    stream.on("data", (evt: RegistryEvent) => {
      const rec: RegistryEventRecord = {
        type: eventTypeToString(evt.type),
        capability: evt.capability ? toRecord(evt.capability) : undefined,
      };
      onEvent(rec);
    });
    if (onError) {
      stream.on("error", onError);
    }
    return stream;
  }
}
