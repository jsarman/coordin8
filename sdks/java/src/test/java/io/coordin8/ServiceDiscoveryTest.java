package io.coordin8;

import com.google.protobuf.Empty;
import coordin8.LeaseOuterClass.Lease;
import coordin8.LeaseOuterClass.RenewRequest;
import coordin8.LeaseServiceGrpc;
import coordin8.Proxy.*;
import coordin8.ProxyServiceGrpc;
import coordin8.Registry.*;
import coordin8.RegistryServiceGrpc;
import io.coordin8.TestUtil.Harness;
import io.grpc.ManagedChannel;
import io.grpc.Status;
import io.grpc.stub.StreamObserver;
import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.BeforeEach;
import org.junit.jupiter.api.Test;

import java.util.List;
import java.util.Map;
import java.util.concurrent.CopyOnWriteArrayList;
import java.util.concurrent.atomic.AtomicInteger;

import static org.junit.jupiter.api.Assertions.*;

class ServiceDiscoveryTest {

    /** Registry whose Watch streams stay open so tests can push events. */
    static class PushRegistry extends RegistryServiceGrpc.RegistryServiceImplBase {
        final List<StreamObserver<RegistryEvent>> watchers = new CopyOnWriteArrayList<>();
        final AtomicInteger watchCount = new AtomicInteger();

        @Override
        public void watch(RegistryWatchRequest req, StreamObserver<RegistryEvent> out) {
            watchers.add(out);
            watchCount.incrementAndGet();
        }

        void push(RegistryEvent.EventType type) {
            RegistryEvent evt = RegistryEvent.newBuilder().setType(type)
                    .setCapability(Capability.newBuilder().setCapabilityId("c").setInterface("Greeter")).build();
            for (var w : watchers) w.onNext(evt);
        }
    }

    static class FakeProxy extends ProxyServiceGrpc.ProxyServiceImplBase {
        final AtomicInteger opens = new AtomicInteger();
        final List<String> released = new CopyOnWriteArrayList<>();
        final List<Map<String, String>> templates = new CopyOnWriteArrayList<>();
        final List<Long> requestedTtls = new CopyOnWriteArrayList<>();
        /** Lease TTL (seconds) the fake grants; 0 = grant no lease. */
        volatile long leaseTtl = 0;
        volatile int port = 1; // channel is lazy; the port never has to be live

        @Override
        public void open(OpenRequest req, StreamObserver<ProxyHandle> out) {
            templates.add(req.getTemplateMap());
            requestedTtls.add(req.getTtlSeconds());
            int n = opens.incrementAndGet();
            ProxyHandle.Builder b = ProxyHandle.newBuilder().setProxyId("proxy-" + n).setLocalPort(port);
            if (leaseTtl > 0) {
                b.setLease(Lease.newBuilder().setLeaseId("please-" + n).setResourceId("proxy-" + n)
                        .setTtlSeconds(leaseTtl));
            }
            out.onNext(b.build());
            out.onCompleted();
        }

        @Override
        public void release(ReleaseRequest req, StreamObserver<Empty> out) {
            released.add(req.getProxyId());
            out.onNext(Empty.getDefaultInstance());
            out.onCompleted();
        }
    }

    /** LeaseService mounted next to the fake Proxy (Proxy grants its own leases). */
    static class FakeProxyLease extends LeaseServiceGrpc.LeaseServiceImplBase {
        final List<String> renewed = new CopyOnWriteArrayList<>();
        volatile java.util.function.IntFunction<Status> behavior = n -> Status.OK;

        @Override
        public void renew(RenewRequest req, StreamObserver<Lease> out) {
            renewed.add(req.getLeaseId());
            Status st = behavior.apply(renewed.size());
            if (!st.isOk()) {
                out.onError(st.asRuntimeException());
                return;
            }
            out.onNext(Lease.newBuilder().setLeaseId(req.getLeaseId()).setTtlSeconds(req.getTtlSeconds()).build());
            out.onCompleted();
        }
    }

    static final Map<String, String> TEMPLATE = Map.of("interface", "Greeter");

    Harness h;
    PushRegistry registry;
    FakeProxy proxy;
    FakeProxyLease proxyLease;
    DjinnClient djinn;
    ServiceDiscovery discovery;

    @BeforeEach
    void setUp() throws Exception {
        h = new Harness();
        registry = new PushRegistry();
        proxy = new FakeProxy();
        proxyLease = new FakeProxyLease();
        int port = h.tcp(registry, proxy, proxyLease);
        String addr = "localhost:" + port;
        // Pin everything to the one fake server; Space/Event channels are never used.
        djinn = DjinnClient.connect(addr, addr, addr, addr);
        discovery = ServiceDiscovery.watch(djinn);
    }

    @AfterEach
    void tearDown() throws Exception {
        discovery.close();
        djinn.close();
        h.close();
    }

    @Test
    void repeatedGetWithSameTemplateReusesCachedChannel() throws Exception {
        ManagedChannel a = discovery.get(c -> c, TEMPLATE);
        ManagedChannel b = discovery.get(c -> c, Map.of("interface", "Greeter"));
        assertSame(a, b);
        assertEquals(1, proxy.opens.get());
        assertEquals(TEMPLATE, proxy.templates.get(0));
        assertTrue(TestUtil.await(3000, () -> registry.watchCount.get() == 1));
    }

    @Test
    void templateKeyIsOrderIndependent() {
        var t1 = new java.util.LinkedHashMap<String, String>();
        t1.put("interface", "Greeter");
        t1.put("lang", "en");
        var t2 = new java.util.LinkedHashMap<String, String>();
        t2.put("lang", "en");
        t2.put("interface", "Greeter");
        assertSame(discovery.get(c -> c, t1), discovery.get(c -> c, t2));
        assertEquals(1, proxy.opens.get());
    }

    @Test
    void differentTemplatesGetSeparateProxies() {
        ManagedChannel a = discovery.get(c -> c, TEMPLATE);
        ManagedChannel b = discovery.get(c -> c, Map.of("interface", "Other"));
        assertNotSame(a, b);
        assertEquals(2, proxy.opens.get());
    }

    @Test
    void expiredMarksStaleAndNextGetReopensAndRetainsOld() throws Exception {
        ManagedChannel first = discovery.get(c -> c, TEMPLATE);
        assertTrue(TestUtil.await(3000, () -> !registry.watchers.isEmpty()));

        registry.push(RegistryEvent.EventType.EXPIRED);
        // The event is handled asynchronously; keep asking until the cache notices.
        ManagedChannel[] second = new ManagedChannel[1];
        assertTrue(TestUtil.await(5000, () -> {
            second[0] = discovery.get(c -> c, TEMPLATE);
            return proxy.opens.get() == 2;
        }));
        assertNotSame(first, second[0]);
        // A caller may still hold the old channel: it stays open until close().
        assertTrue(proxy.released.isEmpty());
        assertFalse(first.isShutdown(), "old channel must stay usable");
        assertFalse(second[0].isShutdown());
        discovery.close();
        assertEquals(2, proxy.released.size());
        assertTrue(first.isShutdown());
    }

    @Test
    void registeredAfterExpiredEagerlyRefreshesWithoutGet() throws Exception {
        discovery.get(c -> c, TEMPLATE);
        assertTrue(TestUtil.await(3000, () -> !registry.watchers.isEmpty()));

        registry.push(RegistryEvent.EventType.EXPIRED);
        registry.push(RegistryEvent.EventType.REGISTERED);
        assertTrue(TestUtil.await(5000, () -> proxy.opens.get() == 2), "eager refresh on register");
        assertTrue(proxy.released.isEmpty(), "old proxy is retained until close()");
    }

    @Test
    void registeredWhileFreshDoesNothing() throws Exception {
        discovery.get(c -> c, TEMPLATE);
        assertTrue(TestUtil.await(3000, () -> !registry.watchers.isEmpty()));
        registry.push(RegistryEvent.EventType.REGISTERED);
        Thread.sleep(300);
        assertEquals(1, proxy.opens.get());
        assertTrue(proxy.released.isEmpty());
    }

    @Test
    void closeReleasesAllProxiesAndShutsChannels() throws Exception {
        ManagedChannel a = discovery.get(c -> c, TEMPLATE);
        ManagedChannel b = discovery.get(c -> c, Map.of("interface", "Other"));
        discovery.close();
        assertEquals(2, proxy.released.size());
        assertTrue(proxy.released.containsAll(List.of("proxy-1", "proxy-2")));
        assertTrue(a.isShutdown());
        assertTrue(b.isShutdown());
    }

    /** "modified" must neither close the held channel nor reopen: Proxy re-resolves per connection. */
    @Test
    void modifiedEventDoesNotCloseCallersHeldChannel() throws Exception {
        ManagedChannel held = discovery.get(c -> c, TEMPLATE);
        assertTrue(TestUtil.await(3000, () -> !registry.watchers.isEmpty()));
        registry.push(RegistryEvent.EventType.MODIFIED);
        Thread.sleep(300);
        assertFalse(held.isShutdown());
        assertEquals(1, proxy.opens.get());
        assertTrue(proxy.released.isEmpty());
        assertSame(held, discovery.get(c -> c, TEMPLATE));
    }

    // ---- proxy-lease keep-alive ----

    @Test
    void openSendsTtlAndHandleExposesLease() {
        proxy.leaseTtl = 30;
        try (var handle = djinn.proxy().open(TEMPLATE, 45)) {
            assertEquals(List.of(45L), proxy.requestedTtls);
            assertNotNull(handle.lease());
            assertEquals("please-1", handle.lease().leaseId());
            assertFalse(handle.lost().get());
        }
        assertEquals(List.of("proxy-1"), proxy.released);
        try (var handle = djinn.proxy().open(TEMPLATE)) {
            assertEquals(ProxyClient.DEFAULT_PROXY_TTL_SECONDS, proxy.requestedTtls.get(1));
        }
    }

    @Test
    void keepAliveRenewsPeriodicallyAndStopsOnClose() throws Exception {
        proxy.leaseTtl = 2; // renews every 1s
        var handle = djinn.proxy().open(TEMPLATE);
        assertTrue(TestUtil.await(5000, () -> proxyLease.renewed.size() >= 2), "periodic renewals");
        assertTrue(proxyLease.renewed.stream().allMatch("please-1"::equals));
        handle.close();
        Thread.sleep(200);
        int n = proxyLease.renewed.size();
        Thread.sleep(2500);
        assertEquals(n, proxyLease.renewed.size(), "no renewals after close");
        assertFalse(handle.lost().get());
    }

    @Test
    void renewNotFoundMarksEntryStaleAndNextGetReopens() throws Exception {
        proxy.leaseTtl = 2;
        proxyLease.behavior = n -> Status.NOT_FOUND;
        ManagedChannel first = discovery.get(c -> c, TEMPLATE);
        // Lost lease => stale; the next get reopens (keep asking until noticed).
        ManagedChannel[] second = new ManagedChannel[1];
        assertTrue(TestUtil.await(6000, () -> {
            second[0] = discovery.get(c -> c, TEMPLATE);
            return proxy.opens.get() >= 2;
        }));
        assertNotSame(first, second[0]);
        // The lost proxy's channel is dead anyway: torn down on replacement.
        assertTrue(first.isShutdown());
        assertTrue(proxy.released.contains("proxy-1"));
    }

    @Test
    void transientRenewErrorKeepsRenewing() throws Exception {
        proxy.leaseTtl = 2;
        proxyLease.behavior = n -> n <= 2 ? Status.UNAVAILABLE : Status.OK;
        var handle = djinn.proxy().open(TEMPLATE);
        try {
            assertTrue(TestUtil.await(8000, () -> proxyLease.renewed.size() >= 4), "renewals continue past errors");
            assertFalse(handle.lost().get());
        } finally {
            handle.close();
        }
    }
}
