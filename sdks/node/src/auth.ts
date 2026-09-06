import * as grpc from "@grpc/grpc-js";

/**
 * Attaches a static bearer token to every RPC as `authorization: Bearer
 * <token>` metadata — gRPC's call-credentials layer, distinct from (and
 * composable with) transport/channel credentials. See `coordin8-auth` on the
 * Rust side and `.claude/plans/grpc-security/PRD.md` for the full design
 * this implements the Node client half of.
 *
 * This is a client *interceptor*, not `grpc.CallCredentials` —
 * `@grpc/grpc-js`'s `ChannelCredentials.compose()` refuses to combine call
 * credentials with insecure (plaintext) channel credentials at all
 * ("Cannot compose insecure credentials"), unlike Go/Java's per-RPC
 * credential APIs which allow it via an explicit opt-out
 * (`RequireTransportSecurity() == false`). Coordin8 has no TLS channel
 * credentials yet (transport encryption is explicitly deferred, see the
 * PRD's Non-Goals), so an interceptor — which has no such restriction — is
 * the mechanism that actually works here today.
 */
function bearerTokenInterceptor(token: string): grpc.Interceptor {
  return (options: grpc.InterceptorOptions, nextCall: grpc.NextCall) => {
    return new grpc.InterceptingCall(nextCall(options), {
      start(metadata, listener, next) {
        metadata.add("authorization", `Bearer ${token}`);
        next(metadata, listener);
      },
    });
  };
}

/**
 * Builds the interceptor list for a client, given an optional bearer token.
 * Shared by DjinnClient and LeaseClient.dial so both dial paths authenticate
 * identically. Every generated service client this SDK constructs accepts
 * `interceptors` in its options, applied per-call regardless of which
 * `channel`/`channelOverride` it's built on.
 */
export function interceptorsFor(token?: string): grpc.Interceptor[] {
  return token ? [bearerTokenInterceptor(token)] : [];
}
