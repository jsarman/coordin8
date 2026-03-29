import { type CallOptions, ChannelCredentials, Client, type ClientOptions, ClientReadableStream, type ClientUnaryCall, handleServerStreamingCall, type handleUnaryCall, Metadata, type ServiceError, type UntypedServiceImplementation } from "@grpc/grpc-js";
import _m0 from "protobufjs/minimal";
import { Lease } from "./lease";
export declare const protobufPackage = "coordin8";
export interface RegisterRequest {
    interface: string;
    attrs: {
        [key: string]: string;
    };
    ttlSeconds: number;
    transport: TransportDescriptor | undefined;
}
export interface RegisterRequest_AttrsEntry {
    key: string;
    value: string;
}
export interface LookupRequest {
    /**
     * Template: missing fields match anything, present fields must match.
     * "interface" is a required field.
     */
    template: {
        [key: string]: string;
    };
}
export interface LookupRequest_TemplateEntry {
    key: string;
    value: string;
}
export interface RegistryWatchRequest {
    template: {
        [key: string]: string;
    };
}
export interface RegistryWatchRequest_TemplateEntry {
    key: string;
    value: string;
}
export interface Capability {
    capabilityId: string;
    interface: string;
    attrs: {
        [key: string]: string;
    };
    transport: TransportDescriptor | undefined;
}
export interface Capability_AttrsEntry {
    key: string;
    value: string;
}
export interface TransportDescriptor {
    /** type: "grpc", "kafka", "kinesis", "nats", "tcp" */
    type: string;
    /** transport-specific config resolved by the SDK */
    config: {
        [key: string]: string;
    };
}
export interface TransportDescriptor_ConfigEntry {
    key: string;
    value: string;
}
export interface RegistryEvent {
    type: RegistryEvent_EventType;
    capability: Capability | undefined;
}
export declare enum RegistryEvent_EventType {
    REGISTERED = 0,
    EXPIRED = 1,
    RENEWED = 2,
    UNRECOGNIZED = -1
}
export declare function registryEvent_EventTypeFromJSON(object: any): RegistryEvent_EventType;
export declare function registryEvent_EventTypeToJSON(object: RegistryEvent_EventType): string;
export declare const RegisterRequest: {
    encode(message: RegisterRequest, writer?: _m0.Writer): _m0.Writer;
    decode(input: _m0.Reader | Uint8Array, length?: number): RegisterRequest;
    fromJSON(object: any): RegisterRequest;
    toJSON(message: RegisterRequest): unknown;
    create<I extends Exact<DeepPartial<RegisterRequest>, I>>(base?: I): RegisterRequest;
    fromPartial<I extends Exact<DeepPartial<RegisterRequest>, I>>(object: I): RegisterRequest;
};
export declare const RegisterRequest_AttrsEntry: {
    encode(message: RegisterRequest_AttrsEntry, writer?: _m0.Writer): _m0.Writer;
    decode(input: _m0.Reader | Uint8Array, length?: number): RegisterRequest_AttrsEntry;
    fromJSON(object: any): RegisterRequest_AttrsEntry;
    toJSON(message: RegisterRequest_AttrsEntry): unknown;
    create<I extends Exact<DeepPartial<RegisterRequest_AttrsEntry>, I>>(base?: I): RegisterRequest_AttrsEntry;
    fromPartial<I extends Exact<DeepPartial<RegisterRequest_AttrsEntry>, I>>(object: I): RegisterRequest_AttrsEntry;
};
export declare const LookupRequest: {
    encode(message: LookupRequest, writer?: _m0.Writer): _m0.Writer;
    decode(input: _m0.Reader | Uint8Array, length?: number): LookupRequest;
    fromJSON(object: any): LookupRequest;
    toJSON(message: LookupRequest): unknown;
    create<I extends Exact<DeepPartial<LookupRequest>, I>>(base?: I): LookupRequest;
    fromPartial<I extends Exact<DeepPartial<LookupRequest>, I>>(object: I): LookupRequest;
};
export declare const LookupRequest_TemplateEntry: {
    encode(message: LookupRequest_TemplateEntry, writer?: _m0.Writer): _m0.Writer;
    decode(input: _m0.Reader | Uint8Array, length?: number): LookupRequest_TemplateEntry;
    fromJSON(object: any): LookupRequest_TemplateEntry;
    toJSON(message: LookupRequest_TemplateEntry): unknown;
    create<I extends Exact<DeepPartial<LookupRequest_TemplateEntry>, I>>(base?: I): LookupRequest_TemplateEntry;
    fromPartial<I extends Exact<DeepPartial<LookupRequest_TemplateEntry>, I>>(object: I): LookupRequest_TemplateEntry;
};
export declare const RegistryWatchRequest: {
    encode(message: RegistryWatchRequest, writer?: _m0.Writer): _m0.Writer;
    decode(input: _m0.Reader | Uint8Array, length?: number): RegistryWatchRequest;
    fromJSON(object: any): RegistryWatchRequest;
    toJSON(message: RegistryWatchRequest): unknown;
    create<I extends Exact<DeepPartial<RegistryWatchRequest>, I>>(base?: I): RegistryWatchRequest;
    fromPartial<I extends Exact<DeepPartial<RegistryWatchRequest>, I>>(object: I): RegistryWatchRequest;
};
export declare const RegistryWatchRequest_TemplateEntry: {
    encode(message: RegistryWatchRequest_TemplateEntry, writer?: _m0.Writer): _m0.Writer;
    decode(input: _m0.Reader | Uint8Array, length?: number): RegistryWatchRequest_TemplateEntry;
    fromJSON(object: any): RegistryWatchRequest_TemplateEntry;
    toJSON(message: RegistryWatchRequest_TemplateEntry): unknown;
    create<I extends Exact<DeepPartial<RegistryWatchRequest_TemplateEntry>, I>>(base?: I): RegistryWatchRequest_TemplateEntry;
    fromPartial<I extends Exact<DeepPartial<RegistryWatchRequest_TemplateEntry>, I>>(object: I): RegistryWatchRequest_TemplateEntry;
};
export declare const Capability: {
    encode(message: Capability, writer?: _m0.Writer): _m0.Writer;
    decode(input: _m0.Reader | Uint8Array, length?: number): Capability;
    fromJSON(object: any): Capability;
    toJSON(message: Capability): unknown;
    create<I extends Exact<DeepPartial<Capability>, I>>(base?: I): Capability;
    fromPartial<I extends Exact<DeepPartial<Capability>, I>>(object: I): Capability;
};
export declare const Capability_AttrsEntry: {
    encode(message: Capability_AttrsEntry, writer?: _m0.Writer): _m0.Writer;
    decode(input: _m0.Reader | Uint8Array, length?: number): Capability_AttrsEntry;
    fromJSON(object: any): Capability_AttrsEntry;
    toJSON(message: Capability_AttrsEntry): unknown;
    create<I extends Exact<DeepPartial<Capability_AttrsEntry>, I>>(base?: I): Capability_AttrsEntry;
    fromPartial<I extends Exact<DeepPartial<Capability_AttrsEntry>, I>>(object: I): Capability_AttrsEntry;
};
export declare const TransportDescriptor: {
    encode(message: TransportDescriptor, writer?: _m0.Writer): _m0.Writer;
    decode(input: _m0.Reader | Uint8Array, length?: number): TransportDescriptor;
    fromJSON(object: any): TransportDescriptor;
    toJSON(message: TransportDescriptor): unknown;
    create<I extends Exact<DeepPartial<TransportDescriptor>, I>>(base?: I): TransportDescriptor;
    fromPartial<I extends Exact<DeepPartial<TransportDescriptor>, I>>(object: I): TransportDescriptor;
};
export declare const TransportDescriptor_ConfigEntry: {
    encode(message: TransportDescriptor_ConfigEntry, writer?: _m0.Writer): _m0.Writer;
    decode(input: _m0.Reader | Uint8Array, length?: number): TransportDescriptor_ConfigEntry;
    fromJSON(object: any): TransportDescriptor_ConfigEntry;
    toJSON(message: TransportDescriptor_ConfigEntry): unknown;
    create<I extends Exact<DeepPartial<TransportDescriptor_ConfigEntry>, I>>(base?: I): TransportDescriptor_ConfigEntry;
    fromPartial<I extends Exact<DeepPartial<TransportDescriptor_ConfigEntry>, I>>(object: I): TransportDescriptor_ConfigEntry;
};
export declare const RegistryEvent: {
    encode(message: RegistryEvent, writer?: _m0.Writer): _m0.Writer;
    decode(input: _m0.Reader | Uint8Array, length?: number): RegistryEvent;
    fromJSON(object: any): RegistryEvent;
    toJSON(message: RegistryEvent): unknown;
    create<I extends Exact<DeepPartial<RegistryEvent>, I>>(base?: I): RegistryEvent;
    fromPartial<I extends Exact<DeepPartial<RegistryEvent>, I>>(object: I): RegistryEvent;
};
export type RegistryServiceService = typeof RegistryServiceService;
export declare const RegistryServiceService: {
    /** Register a service. Returns a lease — stop renewing to disappear. */
    readonly register: {
        readonly path: "/coordin8.RegistryService/Register";
        readonly requestStream: false;
        readonly responseStream: false;
        readonly requestSerialize: (value: RegisterRequest) => Buffer<ArrayBuffer>;
        readonly requestDeserialize: (value: Buffer) => RegisterRequest;
        readonly responseSerialize: (value: Lease) => Buffer<ArrayBuffer>;
        readonly responseDeserialize: (value: Buffer) => Lease;
    };
    /** Lookup returns the first matching capability. */
    readonly lookup: {
        readonly path: "/coordin8.RegistryService/Lookup";
        readonly requestStream: false;
        readonly responseStream: false;
        readonly requestSerialize: (value: LookupRequest) => Buffer<ArrayBuffer>;
        readonly requestDeserialize: (value: Buffer) => LookupRequest;
        readonly responseSerialize: (value: Capability) => Buffer<ArrayBuffer>;
        readonly responseDeserialize: (value: Buffer) => Capability;
    };
    /** LookupAll streams all currently matching capabilities. */
    readonly lookupAll: {
        readonly path: "/coordin8.RegistryService/LookupAll";
        readonly requestStream: false;
        readonly responseStream: true;
        readonly requestSerialize: (value: LookupRequest) => Buffer<ArrayBuffer>;
        readonly requestDeserialize: (value: Buffer) => LookupRequest;
        readonly responseSerialize: (value: Capability) => Buffer<ArrayBuffer>;
        readonly responseDeserialize: (value: Buffer) => Capability;
    };
    /** Watch streams registration and expiration events for matching capabilities. */
    readonly watch: {
        readonly path: "/coordin8.RegistryService/Watch";
        readonly requestStream: false;
        readonly responseStream: true;
        readonly requestSerialize: (value: RegistryWatchRequest) => Buffer<ArrayBuffer>;
        readonly requestDeserialize: (value: Buffer) => RegistryWatchRequest;
        readonly responseSerialize: (value: RegistryEvent) => Buffer<ArrayBuffer>;
        readonly responseDeserialize: (value: Buffer) => RegistryEvent;
    };
};
export interface RegistryServiceServer extends UntypedServiceImplementation {
    /** Register a service. Returns a lease — stop renewing to disappear. */
    register: handleUnaryCall<RegisterRequest, Lease>;
    /** Lookup returns the first matching capability. */
    lookup: handleUnaryCall<LookupRequest, Capability>;
    /** LookupAll streams all currently matching capabilities. */
    lookupAll: handleServerStreamingCall<LookupRequest, Capability>;
    /** Watch streams registration and expiration events for matching capabilities. */
    watch: handleServerStreamingCall<RegistryWatchRequest, RegistryEvent>;
}
export interface RegistryServiceClient extends Client {
    /** Register a service. Returns a lease — stop renewing to disappear. */
    register(request: RegisterRequest, callback: (error: ServiceError | null, response: Lease) => void): ClientUnaryCall;
    register(request: RegisterRequest, metadata: Metadata, callback: (error: ServiceError | null, response: Lease) => void): ClientUnaryCall;
    register(request: RegisterRequest, metadata: Metadata, options: Partial<CallOptions>, callback: (error: ServiceError | null, response: Lease) => void): ClientUnaryCall;
    /** Lookup returns the first matching capability. */
    lookup(request: LookupRequest, callback: (error: ServiceError | null, response: Capability) => void): ClientUnaryCall;
    lookup(request: LookupRequest, metadata: Metadata, callback: (error: ServiceError | null, response: Capability) => void): ClientUnaryCall;
    lookup(request: LookupRequest, metadata: Metadata, options: Partial<CallOptions>, callback: (error: ServiceError | null, response: Capability) => void): ClientUnaryCall;
    /** LookupAll streams all currently matching capabilities. */
    lookupAll(request: LookupRequest, options?: Partial<CallOptions>): ClientReadableStream<Capability>;
    lookupAll(request: LookupRequest, metadata?: Metadata, options?: Partial<CallOptions>): ClientReadableStream<Capability>;
    /** Watch streams registration and expiration events for matching capabilities. */
    watch(request: RegistryWatchRequest, options?: Partial<CallOptions>): ClientReadableStream<RegistryEvent>;
    watch(request: RegistryWatchRequest, metadata?: Metadata, options?: Partial<CallOptions>): ClientReadableStream<RegistryEvent>;
}
export declare const RegistryServiceClient: {
    new (address: string, credentials: ChannelCredentials, options?: Partial<ClientOptions>): RegistryServiceClient;
    service: typeof RegistryServiceService;
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
