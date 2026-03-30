import * as grpc from "@grpc/grpc-js";
import { LeaseServiceClient } from "../gen/coordin8/lease";
import type { Lease as ProtoLease } from "../gen/coordin8/lease";

export interface Lease {
  leaseId: string;
  resourceId: string;
  grantedAt?: Date;
  expiresAt?: Date;
  ttlSeconds: number;
}

function toLease(r: ProtoLease): Lease {
  return {
    leaseId: r.leaseId,
    resourceId: r.resourceId,
    grantedAt: r.grantedAt ?? undefined,
    expiresAt: r.expiresAt ?? undefined,
    ttlSeconds: r.ttlSeconds,
  };
}

export class LeaseClient {
  private readonly stub: LeaseServiceClient;

  constructor(channel: grpc.Channel) {
    this.stub = new LeaseServiceClient(
      "passthrough:///djinn",
      grpc.credentials.createInsecure(),
      { channelOverride: channel }
    );
  }

  grant(resourceId: string, ttlSeconds: number): Promise<Lease> {
    return new Promise((resolve, reject) => {
      this.stub.grant({ resourceId, ttlSeconds }, (err, res) => {
        if (err || !res) return reject(err);
        resolve(toLease(res));
      });
    });
  }

  renew(leaseId: string, ttlSeconds: number): Promise<Lease> {
    return new Promise((resolve, reject) => {
      this.stub.renew({ leaseId, ttlSeconds }, (err, res) => {
        if (err || !res) return reject(err);
        resolve(toLease(res));
      });
    });
  }

  cancel(leaseId: string): Promise<void> {
    return new Promise((resolve, reject) => {
      this.stub.cancel({ leaseId }, (err) => {
        if (err) return reject(err);
        resolve();
      });
    });
  }

  keepAlive(
    leaseId: string,
    ttlSeconds: number,
    signal: AbortSignal
  ): void {
    const intervalMs = (ttlSeconds / 2) * 1000;
    const timer = setInterval(async () => {
      try {
        await this.renew(leaseId, ttlSeconds);
      } catch {
        clearInterval(timer);
      }
    }, intervalMs);
    signal.addEventListener("abort", () => clearInterval(timer));
  }
}
