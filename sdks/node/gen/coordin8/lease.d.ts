import { type CallOptions, ChannelCredentials, Client, type ClientOptions, ClientReadableStream, type ClientUnaryCall, handleServerStreamingCall, type handleUnaryCall, Metadata, type ServiceError, type UntypedServiceImplementation } from "@grpc/grpc-js";
import _m0 from "protobufjs/minimal";
import { Empty } from "../google/protobuf/empty";
export declare const protobufPackage = "coordin8";
export interface GrantRequest {
    resourceId: string;
    ttlSeconds: number;
}
export interface RenewRequest {
    leaseId: string;
    ttlSeconds: number;
}
export interface CancelRequest {
    leaseId: string;
}
export interface WatchExpiryRequest {
    /** Empty = watch all expirations. Set resource_id to watch a specific one. */
    resourceId: string;
}
export interface Lease {
    leaseId: string;
    resourceId: string;
    grantedAt: Date | undefined;
    expiresAt: Date | undefined;
}
export interface ExpiryEvent {
    leaseId: string;
    resourceId: string;
    expiredAt: Date | undefined;
}
export declare const GrantRequest: {
    encode(message: GrantRequest, writer?: _m0.Writer): _m0.Writer;
    decode(input: _m0.Reader | Uint8Array, length?: number): GrantRequest;
    fromJSON(object: any): GrantRequest;
    toJSON(message: GrantRequest): unknown;
    create<I extends Exact<DeepPartial<GrantRequest>, I>>(base?: I): GrantRequest;
    fromPartial<I extends Exact<DeepPartial<GrantRequest>, I>>(object: I): GrantRequest;
};
export declare const RenewRequest: {
    encode(message: RenewRequest, writer?: _m0.Writer): _m0.Writer;
    decode(input: _m0.Reader | Uint8Array, length?: number): RenewRequest;
    fromJSON(object: any): RenewRequest;
    toJSON(message: RenewRequest): unknown;
    create<I extends Exact<DeepPartial<RenewRequest>, I>>(base?: I): RenewRequest;
    fromPartial<I extends Exact<DeepPartial<RenewRequest>, I>>(object: I): RenewRequest;
};
export declare const CancelRequest: {
    encode(message: CancelRequest, writer?: _m0.Writer): _m0.Writer;
    decode(input: _m0.Reader | Uint8Array, length?: number): CancelRequest;
    fromJSON(object: any): CancelRequest;
    toJSON(message: CancelRequest): unknown;
    create<I extends Exact<DeepPartial<CancelRequest>, I>>(base?: I): CancelRequest;
    fromPartial<I extends Exact<DeepPartial<CancelRequest>, I>>(object: I): CancelRequest;
};
export declare const WatchExpiryRequest: {
    encode(message: WatchExpiryRequest, writer?: _m0.Writer): _m0.Writer;
    decode(input: _m0.Reader | Uint8Array, length?: number): WatchExpiryRequest;
    fromJSON(object: any): WatchExpiryRequest;
    toJSON(message: WatchExpiryRequest): unknown;
    create<I extends Exact<DeepPartial<WatchExpiryRequest>, I>>(base?: I): WatchExpiryRequest;
    fromPartial<I extends Exact<DeepPartial<WatchExpiryRequest>, I>>(object: I): WatchExpiryRequest;
};
export declare const Lease: {
    encode(message: Lease, writer?: _m0.Writer): _m0.Writer;
    decode(input: _m0.Reader | Uint8Array, length?: number): Lease;
    fromJSON(object: any): Lease;
    toJSON(message: Lease): unknown;
    create<I extends Exact<DeepPartial<Lease>, I>>(base?: I): Lease;
    fromPartial<I extends Exact<DeepPartial<Lease>, I>>(object: I): Lease;
};
export declare const ExpiryEvent: {
    encode(message: ExpiryEvent, writer?: _m0.Writer): _m0.Writer;
    decode(input: _m0.Reader | Uint8Array, length?: number): ExpiryEvent;
    fromJSON(object: any): ExpiryEvent;
    toJSON(message: ExpiryEvent): unknown;
    create<I extends Exact<DeepPartial<ExpiryEvent>, I>>(base?: I): ExpiryEvent;
    fromPartial<I extends Exact<DeepPartial<ExpiryEvent>, I>>(object: I): ExpiryEvent;
};
export type LeaseServiceService = typeof LeaseServiceService;
export declare const LeaseServiceService: {
    readonly grant: {
        readonly path: "/coordin8.LeaseService/Grant";
        readonly requestStream: false;
        readonly responseStream: false;
        readonly requestSerialize: (value: GrantRequest) => Buffer<ArrayBuffer>;
        readonly requestDeserialize: (value: Buffer) => GrantRequest;
        readonly responseSerialize: (value: Lease) => Buffer<ArrayBuffer>;
        readonly responseDeserialize: (value: Buffer) => Lease;
    };
    readonly renew: {
        readonly path: "/coordin8.LeaseService/Renew";
        readonly requestStream: false;
        readonly responseStream: false;
        readonly requestSerialize: (value: RenewRequest) => Buffer<ArrayBuffer>;
        readonly requestDeserialize: (value: Buffer) => RenewRequest;
        readonly responseSerialize: (value: Lease) => Buffer<ArrayBuffer>;
        readonly responseDeserialize: (value: Buffer) => Lease;
    };
    readonly cancel: {
        readonly path: "/coordin8.LeaseService/Cancel";
        readonly requestStream: false;
        readonly responseStream: false;
        readonly requestSerialize: (value: CancelRequest) => Buffer<ArrayBuffer>;
        readonly requestDeserialize: (value: Buffer) => CancelRequest;
        readonly responseSerialize: (value: Empty) => Buffer<ArrayBuffer>;
        readonly responseDeserialize: (value: Buffer) => Empty;
    };
    readonly watchExpiry: {
        readonly path: "/coordin8.LeaseService/WatchExpiry";
        readonly requestStream: false;
        readonly responseStream: true;
        readonly requestSerialize: (value: WatchExpiryRequest) => Buffer<ArrayBuffer>;
        readonly requestDeserialize: (value: Buffer) => WatchExpiryRequest;
        readonly responseSerialize: (value: ExpiryEvent) => Buffer<ArrayBuffer>;
        readonly responseDeserialize: (value: Buffer) => ExpiryEvent;
    };
};
export interface LeaseServiceServer extends UntypedServiceImplementation {
    grant: handleUnaryCall<GrantRequest, Lease>;
    renew: handleUnaryCall<RenewRequest, Lease>;
    cancel: handleUnaryCall<CancelRequest, Empty>;
    watchExpiry: handleServerStreamingCall<WatchExpiryRequest, ExpiryEvent>;
}
export interface LeaseServiceClient extends Client {
    grant(request: GrantRequest, callback: (error: ServiceError | null, response: Lease) => void): ClientUnaryCall;
    grant(request: GrantRequest, metadata: Metadata, callback: (error: ServiceError | null, response: Lease) => void): ClientUnaryCall;
    grant(request: GrantRequest, metadata: Metadata, options: Partial<CallOptions>, callback: (error: ServiceError | null, response: Lease) => void): ClientUnaryCall;
    renew(request: RenewRequest, callback: (error: ServiceError | null, response: Lease) => void): ClientUnaryCall;
    renew(request: RenewRequest, metadata: Metadata, callback: (error: ServiceError | null, response: Lease) => void): ClientUnaryCall;
    renew(request: RenewRequest, metadata: Metadata, options: Partial<CallOptions>, callback: (error: ServiceError | null, response: Lease) => void): ClientUnaryCall;
    cancel(request: CancelRequest, callback: (error: ServiceError | null, response: Empty) => void): ClientUnaryCall;
    cancel(request: CancelRequest, metadata: Metadata, callback: (error: ServiceError | null, response: Empty) => void): ClientUnaryCall;
    cancel(request: CancelRequest, metadata: Metadata, options: Partial<CallOptions>, callback: (error: ServiceError | null, response: Empty) => void): ClientUnaryCall;
    watchExpiry(request: WatchExpiryRequest, options?: Partial<CallOptions>): ClientReadableStream<ExpiryEvent>;
    watchExpiry(request: WatchExpiryRequest, metadata?: Metadata, options?: Partial<CallOptions>): ClientReadableStream<ExpiryEvent>;
}
export declare const LeaseServiceClient: {
    new (address: string, credentials: ChannelCredentials, options?: Partial<ClientOptions>): LeaseServiceClient;
    service: typeof LeaseServiceService;
    serviceName: string;
};
type Builtin = Date | Function | Uint8Array | string | number | boolean | undefined;
export type DeepPartial<T> = T extends Builtin ? T : T extends globalThis.Array<infer U> ? globalThis.Array<DeepPartial<U>> : T extends ReadonlyArray<infer U> ? ReadonlyArray<DeepPartial<U>> : T extends {} ? {
    [K in keyof T]?: DeepPartial<T[K]>;
} : Partial<T>;
type KeysOfUnion<T> = T extends T ? keyof T : never;
export type Exact<P, I extends P> = P extends Builtin ? P : P & {
    [K in keyof P]: Exact<P[K], I[K]>;
} & {
    [K in Exclude<keyof I, KeysOfUnion<P>>]: never;
};
export {};
