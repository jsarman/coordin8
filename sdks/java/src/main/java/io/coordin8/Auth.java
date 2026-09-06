package io.coordin8;

import io.grpc.ClientInterceptor;
import io.grpc.Metadata;
import io.grpc.stub.MetadataUtils;

/**
 * Attaches a static bearer token to every RPC as {@code authorization: Bearer
 * <token>} metadata — gRPC's call-credentials layer, distinct from (and
 * composable with) transport/channel credentials. See {@code coordin8-auth}
 * on the Rust side and {@code .claude/plans/grpc-security/PRD.md} for the
 * full design this implements the Java client half of.
 *
 * <p>A fixed-header interceptor (rather than {@link io.grpc.CallCredentials})
 * is enough for v1's static, CLI-minted tokens (Decision 2) — there is no
 * per-call token refresh to support yet.
 */
final class Auth {

    private Auth() {}

    static final Metadata.Key<String> AUTHORIZATION =
            Metadata.Key.of("authorization", Metadata.ASCII_STRING_MARSHALLER);

    /** {@code token} must be non-null and non-empty; callers check that first. */
    static ClientInterceptor bearerToken(String token) {
        Metadata headers = new Metadata();
        headers.put(AUTHORIZATION, "Bearer " + token);
        return MetadataUtils.newAttachHeadersInterceptor(headers);
    }
}
