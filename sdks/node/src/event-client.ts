import * as grpc from "@grpc/grpc-js";
import { EventServiceClient as GrpcEventClient } from "../gen/coordin8/event";
import type { Event as ProtoEvent } from "../gen/coordin8/event";
import type { Lease } from "./lease-client";

export interface EventRegistration {
  registrationId: string;
  source: string;
  leaseId?: string;
  seqNum: number;
}

export interface EventRecord {
  eventId: string;
  source: string;
  eventType: string;
  seqNum: number;
  attrs: Record<string, string>;
  payload: Buffer;
  handback: Buffer;
  emittedAt?: Date;
}

export interface SubscribeOpts {
  source: string;
  template?: Record<string, string>;
  durable?: boolean;
  ttlSeconds: number;
  handback?: Buffer;
}

function toEvent(evt: ProtoEvent): EventRecord {
  return {
    eventId: evt.eventId,
    source: evt.source,
    eventType: evt.eventType,
    seqNum: evt.seqNum,
    attrs: evt.attrs ?? {},
    payload: evt.payload,
    handback: evt.handback,
    emittedAt: evt.emittedAt ?? undefined,
  };
}

export class EventClient {
  private readonly stub: GrpcEventClient;

  constructor(channel: grpc.Channel) {
    this.stub = new GrpcEventClient(
      "passthrough:///djinn",
      grpc.credentials.createInsecure(),
      { channelOverride: channel }
    );
  }

  /** Subscribe to events from a source matching a template. */
  subscribe(opts: SubscribeOpts): Promise<EventRegistration> {
    return new Promise((resolve, reject) => {
      this.stub.subscribe(
        {
          source: opts.source,
          template: opts.template ?? {},
          delivery: opts.durable ? 0 : 1, // DURABLE=0, BEST_EFFORT=1
          ttlSeconds: opts.ttlSeconds,
          handback: opts.handback ?? Buffer.alloc(0),
        },
        (err, res) => {
          if (err || !res) return reject(err);
          resolve({
            registrationId: res.registrationId,
            source: res.source,
            leaseId: res.lease?.leaseId || undefined,
            seqNum: res.seqNum,
          });
        }
      );
    });
  }

  /**
   * Receive events for a registration (server-streaming).
   * Returns an async iterable of EventRecord objects.
   */
  async *receive(
    registrationId: string,
    signal?: AbortSignal
  ): AsyncGenerator<EventRecord> {
    const stream = this.stub.receive({ registrationId });
    if (signal) {
      signal.addEventListener("abort", () => stream.cancel(), { once: true });
    }
    for await (const raw of stream) {
      yield toEvent(raw);
    }
  }

  /** Emit an event into the bus. */
  emit(
    source: string,
    eventType: string,
    attrs?: Record<string, string>,
    payload?: Buffer
  ): Promise<void> {
    return new Promise((resolve, reject) => {
      this.stub.emit(
        {
          source,
          eventType,
          attrs: attrs ?? {},
          payload: payload ?? Buffer.alloc(0),
        },
        (err) => {
          if (err) return reject(err);
          resolve();
        }
      );
    });
  }

  /** Renew a subscription lease. */
  renewSubscription(
    registrationId: string,
    ttlSeconds: number
  ): Promise<Lease> {
    return new Promise((resolve, reject) => {
      this.stub.renewSubscription(
        { registrationId, ttlSeconds },
        (err, res) => {
          if (err || !res) return reject(err);
          resolve({
            leaseId: res.leaseId,
            resourceId: res.resourceId,
            grantedAt: res.grantedAt ?? undefined,
            expiresAt: res.expiresAt ?? undefined,
            ttlSeconds: res.ttlSeconds,
          });
        }
      );
    });
  }

  /** Cancel a subscription. */
  cancelSubscription(registrationId: string): Promise<void> {
    return new Promise((resolve, reject) => {
      this.stub.cancelSubscription({ registrationId }, (err) => {
        if (err) return reject(err);
        resolve();
      });
    });
  }
}
