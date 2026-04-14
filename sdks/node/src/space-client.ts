import * as grpc from "@grpc/grpc-js";
import { SpaceServiceClient as GrpcSpaceClient } from "../gen/coordin8/space";
import type {
  Tuple as ProtoTuple,
  SpaceEvent as ProtoSpaceEvent,
  WriteRequest,
} from "../gen/coordin8/space";
import { SpaceEventType } from "../gen/coordin8/space";
import type { Lease } from "./lease-client";

export interface TupleRecord {
  tupleId: string;
  attrs: Record<string, string>;
  payload: Buffer;
  leaseId?: string;
  writtenBy?: string;
  writtenAt?: Date;
  inputTupleId?: string;
}

export interface SpaceEvent {
  type: "appearance" | "expiration";
  tuple?: TupleRecord;
  occurredAt?: Date;
  handback: Buffer;
}

export interface WriteOpts {
  attrs: Record<string, string>;
  payload?: Buffer | string;
  ttlSeconds: number;
  writtenBy?: string;
  inputTupleId?: string;
  txnId?: string;
}

export interface ReadOpts {
  template: Record<string, string>;
  wait?: boolean;
  timeoutMs?: number;
  txnId?: string;
}

export interface TakeOpts {
  template: Record<string, string>;
  wait?: boolean;
  timeoutMs?: number;
  txnId?: string;
}

export interface NotifyOpts {
  template: Record<string, string>;
  on?: "appearance" | "expiration";
  ttlSeconds?: number;
  handback?: Buffer;
}

function toTuple(t: ProtoTuple): TupleRecord {
  return {
    tupleId: t.tupleId,
    attrs: t.attrs ?? {},
    payload: t.payload,
    leaseId: t.lease?.leaseId || undefined,
    writtenBy: t.provenance?.writtenBy || undefined,
    writtenAt: t.provenance?.writtenAt ?? undefined,
    inputTupleId: t.provenance?.inputTupleId || undefined,
  };
}

function toSpaceEvent(evt: ProtoSpaceEvent): SpaceEvent {
  return {
    type: evt.type === SpaceEventType.EXPIRATION ? "expiration" : "appearance",
    tuple: evt.tuple ? toTuple(evt.tuple) : undefined,
    occurredAt: evt.occurredAt ?? undefined,
    handback: evt.handback,
  };
}

export class SpaceClient {
  private readonly stub: GrpcSpaceClient;

  constructor(channel: grpc.Channel) {
    this.stub = new GrpcSpaceClient(
      "passthrough:///djinn",
      grpc.credentials.createInsecure(),
      { channelOverride: channel }
    );
  }

  /** Write a leased tuple into the Space. */
  write(opts: WriteOpts): Promise<TupleRecord> {
    const payload =
      typeof opts.payload === "string"
        ? Buffer.from(opts.payload)
        : opts.payload ?? Buffer.alloc(0);
    return new Promise((resolve, reject) => {
      this.stub.write(
        {
          attrs: opts.attrs,
          payload,
          ttlSeconds: opts.ttlSeconds,
          writtenBy: opts.writtenBy ?? "",
          inputTupleId: opts.inputTupleId ?? "",
          txnId: opts.txnId ?? "",
        },
        (err, res) => {
          if (err || !res) return reject(err);
          resolve(toTuple(res.tuple!));
        }
      );
    });
  }

  /** Non-destructive read by template. Returns null if no match. */
  read(opts: ReadOpts): Promise<TupleRecord | null> {
    return new Promise((resolve, reject) => {
      this.stub.read(
        {
          template: opts.template,
          wait: opts.wait ?? false,
          timeoutMs: opts.timeoutMs ?? 0,
          txnId: opts.txnId ?? "",
        },
        (err, res) => {
          if (err) return reject(err);
          resolve(res?.tuple ? toTuple(res.tuple) : null);
        }
      );
    });
  }

  /** Atomically claim and remove a matching tuple. Returns null if no match. */
  take(opts: TakeOpts): Promise<TupleRecord | null> {
    return new Promise((resolve, reject) => {
      this.stub.take(
        {
          template: opts.template,
          wait: opts.wait ?? false,
          timeoutMs: opts.timeoutMs ?? 0,
          txnId: opts.txnId ?? "",
        },
        (err, res) => {
          if (err) return reject(err);
          resolve(res?.tuple ? toTuple(res.tuple) : null);
        }
      );
    });
  }

  /** Bulk read — returns all tuples matching the template. */
  contents(
    template: Record<string, string>,
    txnId?: string
  ): Promise<TupleRecord[]> {
    return new Promise((resolve, reject) => {
      const results: TupleRecord[] = [];
      const stream = this.stub.contents({
        template,
        txnId: txnId ?? "",
      });
      stream.on("data", (t: ProtoTuple) => results.push(toTuple(t)));
      stream.on("error", (err: Error) => reject(err));
      stream.on("end", () => resolve(results));
    });
  }

  /**
   * Watch for tuple events matching the template.
   * Returns an EventEmitter-like stream. Use `.on("data", ...)` for events.
   * Cancel by calling `.cancel()` on the returned stream or aborting the signal.
   */
  notify(opts: NotifyOpts): grpc.ClientReadableStream<ProtoSpaceEvent> {
    const on =
      opts.on === "expiration"
        ? SpaceEventType.EXPIRATION
        : SpaceEventType.APPEARANCE;
    return this.stub.notify({
      template: opts.template,
      on,
      ttlSeconds: opts.ttlSeconds ?? 60,
      handback: opts.handback ?? Buffer.alloc(0),
      txnId: "",
    });
  }

  /**
   * Higher-level notify that returns an async iterable of SpaceEvent objects.
   * Stops when the stream ends, errors, or the AbortSignal fires.
   */
  async *watch(
    opts: NotifyOpts,
    signal?: AbortSignal
  ): AsyncGenerator<SpaceEvent> {
    const stream = this.notify(opts);
    if (signal) {
      signal.addEventListener("abort", () => stream.cancel(), { once: true });
    }
    for await (const raw of stream) {
      yield toSpaceEvent(raw);
    }
  }

  /** Renew a tuple's lease. */
  renewTuple(tupleId: string, ttlSeconds: number): Promise<Lease> {
    return new Promise((resolve, reject) => {
      this.stub.renew({ tupleId, ttlSeconds }, (err, res) => {
        if (err || !res) return reject(err);
        resolve({
          leaseId: res.leaseId,
          resourceId: res.resourceId,
          grantedAt: res.grantedAt ?? undefined,
          expiresAt: res.expiresAt ?? undefined,
          ttlSeconds: res.ttlSeconds,
        });
      });
    });
  }

  /** Cancel (remove) a tuple and its lease. */
  cancelTuple(tupleId: string): Promise<void> {
    return new Promise((resolve, reject) => {
      this.stub.cancel({ tupleId }, (err) => {
        if (err) return reject(err);
        resolve();
      });
    });
  }
}
