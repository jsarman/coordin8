import * as grpc from "@grpc/grpc-js";
import { LeaseServiceClient } from "../gen/coordin8/lease";
import type { Lease as ProtoLease } from "../gen/coordin8/lease";
import { interceptorsFor } from "./auth";

/**
 * There is no single "the" LeaseMgr to connect to — Registry, Space, and
 * EventMgr each grant their own leases and mount LeaseService on their own
 * connection. Build a LeaseClient with `fromChannel` (given an existing
 * connection you already hold, e.g. `DjinnClient.registryLeases()`) or
 * `dial`/`dialLease` (given a grantor address, typically read off a
 * `Lease`'s `grantorHost`/`grantorPort`). See
 * .claude/plans/distributed-leasing/PRD.md.
 */
export interface Lease {
  leaseId: string;
  resourceId: string;
  grantedAt?: Date;
  expiresAt?: Date;
  ttlSeconds: number;
  /** Where to renew/cancel this lease directly — see grantorAddr(). */
  grantorHost: string;
  grantorPort: number;
}

/** Returns "host:port" for renewing/cancelling this lease — pass it to dialLease(). */
export function grantorAddr(lease: Lease): string {
  return `${lease.grantorHost}:${lease.grantorPort}`;
}

function toLease(r: ProtoLease): Lease {
  return {
    leaseId: r.leaseId,
    resourceId: r.resourceId,
    grantedAt: r.grantedAt ?? undefined,
    expiresAt: r.expiresAt ?? undefined,
    ttlSeconds: r.ttlSeconds,
    grantorHost: r.grantorHost,
    grantorPort: r.grantorPort,
  };
}

export class LeaseClient {
  private readonly stub: LeaseServiceClient;
  private readonly ownsConnection: boolean;

  private constructor(stub: LeaseServiceClient, ownsConnection: boolean) {
    this.stub = stub;
    this.ownsConnection = ownsConnection;
  }

  /**
   * Wrap an existing gRPC channel (e.g. Registry's) as a LeaseClient. Does
   * not take ownership of the channel — the caller is still responsible for
   * closing whatever owns it. This is the common case: every self-registered
   * lease is Registry-granted, so `DjinnClient.registryLeases()` builds one
   * this way.
   */
  static fromChannel(channel: grpc.Channel, interceptors: grpc.Interceptor[] = []): LeaseClient {
    const stub = new LeaseServiceClient(
      "passthrough:///djinn",
      grpc.credentials.createInsecure(),
      { channelOverride: channel, interceptors }
    );
    return new LeaseClient(stub, false);
  }

  /**
   * Dial a lease grantor directly at grantorAddr — typically
   * `grantorHost:grantorPort` read off a Lease you already hold (see
   * grantorAddr()), for leases granted by Space or EventMgr instead of
   * Registry. The returned LeaseClient owns the connection; call close()
   * when done. Pass token if the grantor has COORDIN8_JWT_SECRET set.
   */
  static dial(grantorAddr: string, token?: string): LeaseClient {
    const stub = new LeaseServiceClient(grantorAddr, grpc.credentials.createInsecure(), {
      interceptors: interceptorsFor(token),
    });
    return new LeaseClient(stub, true);
  }

  /**
   * Releases the underlying connection if this LeaseClient owns one (built
   * via dial()/dialLease()). A no-op for one built via fromChannel().
   */
  close(): void {
    if (this.ownsConnection) {
      this.stub.close();
    }
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

  /**
   * Renews leaseId in the background at half the TTL interval. Runs until
   * signal aborts or a renewal fails with the resource genuinely gone.
   *
   * Every failed renewal is reported via onFailure — a transient transport
   * error is retried on the next tick, while NOT_FOUND/FAILED_PRECONDITION
   * (the lease is genuinely gone) is reported and then stops the loop.
   * Callers that don't care about failures can omit onFailure.
   */
  keepAlive(
    leaseId: string,
    ttlSeconds: number,
    signal: AbortSignal,
    onFailure?: (err: unknown) => void
  ): void {
    const intervalMs = (ttlSeconds / 2) * 1000;
    const timer = setInterval(async () => {
      try {
        await this.renew(leaseId, ttlSeconds);
      } catch (err) {
        onFailure?.(err);
        const code = (err as grpc.ServiceError)?.code;
        if (code === grpc.status.NOT_FOUND || code === grpc.status.FAILED_PRECONDITION) {
          // The resource is genuinely gone (LeaseNotFound / LeaseExpired) —
          // no point retrying.
          clearInterval(timer);
          return;
        }
        // Transient failure — keep trying on the next tick.
      }
    }, intervalMs);
    signal.addEventListener("abort", () => clearInterval(timer));
  }
}

/**
 * Dial a lease grantor directly at grantorAddr — mirrors Go's DialLease.
 * Typically called with `grantorHost:grantorPort` read off a Lease you
 * already hold (see grantorAddr()). The returned LeaseClient owns the
 * connection; call close() when done.
 */
export function dialLease(grantorAddr: string, token?: string): LeaseClient {
  return LeaseClient.dial(grantorAddr, token);
}
