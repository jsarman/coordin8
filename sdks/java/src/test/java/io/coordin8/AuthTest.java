package io.coordin8;

import coordin8.LeaseOuterClass.*;
import coordin8.LeaseServiceGrpc;
import io.coordin8.TestUtil.Harness;
import io.grpc.*;
import io.grpc.stub.StreamObserver;
import org.junit.jupiter.api.Test;

import java.util.ArrayList;
import java.util.Collections;
import java.util.List;

import static org.junit.jupiter.api.Assertions.*;

class AuthTest {

    /** Records every `authorization` header the server sees (null if absent). */
    static class HeaderCapture implements ServerInterceptor {
        final List<String> seen = Collections.synchronizedList(new ArrayList<>());

        @Override
        public <A, B> ServerCall.Listener<A> interceptCall(
                ServerCall<A, B> call, Metadata headers, ServerCallHandler<A, B> next) {
            seen.add(headers.get(Auth.AUTHORIZATION));
            return next.startCall(call, headers);
        }
    }

    static class EchoLease extends LeaseServiceGrpc.LeaseServiceImplBase {
        @Override
        public void grant(GrantRequest req, StreamObserver<Lease> out) {
            out.onNext(Lease.newBuilder().setLeaseId("L").build());
            out.onCompleted();
        }
    }

    @Test
    void bearerTokenInterceptorAttachesAuthorizationHeader() throws Exception {
        HeaderCapture cap = new HeaderCapture();
        try (Harness h = new Harness()) {
            int port = h.tcp(cap, new EchoLease());
            ManagedChannel ch = ManagedChannelBuilder.forAddress("localhost", port).usePlaintext()
                    .intercept(Auth.bearerToken("tok123")).build();
            try {
                LeaseClient.wrap(ch).grant("r", 5);
            } finally {
                ch.shutdownNow();
            }
        }
        assertEquals(List.of("Bearer tok123"), cap.seen);
    }

    @Test
    void leaseDialAttachesTokenWhenGiven() throws Exception {
        HeaderCapture cap = new HeaderCapture();
        try (Harness h = new Harness()) {
            int port = h.tcp(cap, new EchoLease());
            try (LeaseClient c = LeaseClient.dial("localhost:" + port, "secret")) {
                c.grant("r", 5);
            }
        }
        assertEquals(List.of("Bearer secret"), cap.seen);
    }

    @Test
    void leaseDialSendsNoHeaderForNullOrEmptyToken() throws Exception {
        HeaderCapture cap = new HeaderCapture();
        try (Harness h = new Harness()) {
            int port = h.tcp(cap, new EchoLease());
            try (LeaseClient c = LeaseClient.dial("localhost:" + port)) {
                c.grant("r", 5);
            }
            try (LeaseClient c = LeaseClient.dial("localhost:" + port, "")) {
                c.grant("r", 5);
            }
        }
        assertEquals(2, cap.seen.size());
        assertNull(cap.seen.get(0));
        assertNull(cap.seen.get(1));
    }
}
