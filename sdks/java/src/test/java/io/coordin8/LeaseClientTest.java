package io.coordin8;

import coordin8.LeaseOuterClass.*;
import coordin8.LeaseServiceGrpc;
import com.google.protobuf.Empty;
import com.google.protobuf.Timestamp;
import io.coordin8.TestUtil.Harness;
import io.grpc.ManagedChannel;
import io.grpc.Status;
import io.grpc.stub.StreamObserver;
import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.BeforeEach;
import org.junit.jupiter.api.Test;

import java.io.Closeable;
import java.util.List;
import java.util.concurrent.CopyOnWriteArrayList;
import java.util.concurrent.atomic.AtomicInteger;
import java.util.function.Function;

import static org.junit.jupiter.api.Assertions.*;

class LeaseClientTest {

    /** Fake LeaseService; renewBehavior decides each Renew's outcome by call number (1-based). */
    static class FakeLease extends LeaseServiceGrpc.LeaseServiceImplBase {
        final AtomicInteger renews = new AtomicInteger();
        final List<String> cancelled = new CopyOnWriteArrayList<>();
        volatile Function<Integer, Status> renewBehavior = n -> Status.OK;
        volatile GrantRequest lastGrant;
        volatile RenewRequest lastRenew;

        @Override
        public void grant(GrantRequest req, StreamObserver<Lease> out) {
            lastGrant = req;
            out.onNext(Lease.newBuilder().setLeaseId("L1").setResourceId(req.getResourceId())
                    .setTtlSeconds(req.getTtlSeconds())
                    .setGrantedAt(Timestamp.newBuilder().setSeconds(1000))
                    .setExpiresAt(Timestamp.newBuilder().setSeconds(1030))
                    .setGrantorHost("gh").setGrantorPort(1234).build());
            out.onCompleted();
        }

        @Override
        public void renew(RenewRequest req, StreamObserver<Lease> out) {
            lastRenew = req;
            int n = renews.incrementAndGet();
            Status st = renewBehavior.apply(n);
            if (!st.isOk()) {
                out.onError(st.asRuntimeException());
                return;
            }
            out.onNext(Lease.newBuilder().setLeaseId(req.getLeaseId()).setTtlSeconds(req.getTtlSeconds()).build());
            out.onCompleted();
        }

        @Override
        public void cancel(CancelRequest req, StreamObserver<Empty> out) {
            cancelled.add(req.getLeaseId());
            out.onNext(Empty.getDefaultInstance());
            out.onCompleted();
        }
    }

    Harness h;
    FakeLease fake;
    LeaseClient client;

    @BeforeEach
    void setUp() throws Exception {
        h = new Harness();
        fake = new FakeLease();
        ManagedChannel ch = h.inProcess(fake);
        client = LeaseClient.wrap(ch);
    }

    @AfterEach
    void tearDown() throws Exception {
        h.close();
    }

    @Test
    void grantMapsLeaseToRecord() {
        var rec = client.grant("res", 30);
        assertEquals("res", fake.lastGrant.getResourceId());
        assertEquals(30, fake.lastGrant.getTtlSeconds());
        assertEquals("L1", rec.leaseId());
        assertEquals("res", rec.resourceId());
        assertEquals(1000, rec.grantedAt().getEpochSecond());
        assertEquals(1030, rec.expiresAt().getEpochSecond());
        assertEquals(30, rec.ttlSeconds());
        assertEquals("gh:1234", rec.grantorAddr());
    }

    @Test
    void toRecordLeavesMissingTimestampsNull() {
        var rec = LeaseClient.toRecord(Lease.newBuilder().setLeaseId("x").build());
        assertNull(rec.grantedAt());
        assertNull(rec.expiresAt());
    }

    @Test
    void renewAndCancelSendIds() {
        client.renew("L9", 15);
        assertEquals("L9", fake.lastRenew.getLeaseId());
        assertEquals(15, fake.lastRenew.getTtlSeconds());
        client.cancel("L9");
        assertEquals(List.of("L9"), fake.cancelled);
    }

    @Test
    void dialRejectsAddressWithoutPort() {
        assertThrows(IllegalArgumentException.class, () -> LeaseClient.dial("no-port"));
    }

    @Test
    void wrappedClientCloseDoesNotShutDownChannel() throws Exception {
        ManagedChannel ch = h.inProcess(fake);
        LeaseClient c = LeaseClient.wrap(ch);
        c.close();
        assertFalse(ch.isShutdown());
        c.renew("L", 5); // still usable
    }

    @Test
    void dialedClientOwnsAndClosesItsChannel() throws Exception {
        int port = h.tcp(fake);
        try (LeaseClient c = LeaseClient.dial("localhost:" + port)) {
            assertEquals("L1", c.grant("r", 10).leaseId());
        }
    }

    @Test
    void keepAliveRenewsOnSchedule() throws Exception {
        List<Throwable> failures = new CopyOnWriteArrayList<>();
        try (Closeable ka = client.keepAlive("L1", 2, failures::add)) {
            assertTrue(TestUtil.await(6000, () -> fake.renews.get() >= 2), "expected >=2 renewals");
        }
        assertEquals("L1", fake.lastRenew.getLeaseId());
        assertEquals(2, fake.lastRenew.getTtlSeconds());
        assertTrue(failures.isEmpty());
    }

    @Test
    void keepAliveStopsOnNotFound() throws Exception {
        assertStopsOn(Status.NOT_FOUND);
    }

    @Test
    void keepAliveStopsOnFailedPrecondition() throws Exception {
        assertStopsOn(Status.FAILED_PRECONDITION);
    }

    private void assertStopsOn(Status status) throws Exception {
        fake.renewBehavior = n -> status;
        List<Throwable> failures = new CopyOnWriteArrayList<>();
        Closeable ka = client.keepAlive("L1", 2, failures::add);
        assertTrue(TestUtil.await(4000, () -> failures.size() >= 1));
        Thread.sleep(2500); // would be 2+ more ticks if still running
        assertEquals(1, failures.size(), "terminal failure reported exactly once");
        assertEquals(1, fake.renews.get(), "no further renewals after terminal failure");
        assertEquals(status.getCode(), Status.fromThrowable(failures.get(0)).getCode());
        ka.close();
    }

    @Test
    void keepAliveRetriesOnOtherErrors() throws Exception {
        fake.renewBehavior = n -> n == 1 ? Status.UNAVAILABLE : Status.OK;
        List<Throwable> failures = new CopyOnWriteArrayList<>();
        try (Closeable ka = client.keepAlive("L1", 2, failures::add)) {
            assertTrue(TestUtil.await(6000, () -> fake.renews.get() >= 3), "loop should survive a transient error");
        }
        assertEquals(1, failures.size());
        assertEquals(Status.Code.UNAVAILABLE, Status.fromThrowable(failures.get(0)).getCode());
    }

    @Test
    void keepAliveKeepsRetryingOnPersistentTransientErrors() throws Exception {
        fake.renewBehavior = n -> Status.INTERNAL;
        List<Throwable> failures = new CopyOnWriteArrayList<>();
        try (Closeable ka = client.keepAlive("L1", 2, failures::add)) {
            assertTrue(TestUtil.await(6000, () -> failures.size() >= 2));
        }
    }

    @Test
    void closeStopsKeepAliveAndDoesNotCancelLease() throws Exception {
        Closeable ka = client.keepAlive("L1", 2, t -> {});
        assertTrue(TestUtil.await(4000, () -> fake.renews.get() >= 1));
        ka.close();
        Thread.sleep(200); // let any in-flight renew finish
        int after = fake.renews.get();
        Thread.sleep(2500);
        assertEquals(after, fake.renews.get(), "no renewals after close");
        assertTrue(fake.cancelled.isEmpty(), "close must not cancel the lease");
    }
}
