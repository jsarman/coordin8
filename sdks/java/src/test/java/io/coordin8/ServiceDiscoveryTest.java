package io.coordin8;

import com.google.protobuf.Empty;
import coordin8.Proxy.*;
import coordin8.ProxyServiceGrpc;
import coordin8.Registry.*;
import coordin8.RegistryServiceGrpc;
import io.coordin8.TestUtil.Harness;
import io.grpc.ManagedChannel;
import io.grpc.stub.StreamObserver;
import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.BeforeEach;
import org.junit.jupiter.api.Disabled;
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
        volatile int port = 1; // channel is lazy; the port never has to be live

        @Override
        public void open(OpenRequest req, StreamObserver<ProxyHandle> out) {
            templates.add(req.getTemplateMap());
            out.onNext(ProxyHandle.newBuilder().setProxyId("proxy-" + opens.incrementAndGet())
                    .setLocalPort(port).build());
            out.onCompleted();
        }

        @Override
        public void release(ReleaseRequest req, StreamObserver<Empty> out) {
            released.add(req.getProxyId());
            out.onNext(Empty.getDefaultInstance());
            out.onCompleted();
        }
    }

    static final Map<String, String> TEMPLATE = Map.of("interface", "Greeter");

    Harness h;
    PushRegistry registry;
    FakeProxy proxy;
    DjinnClient djinn;
    ServiceDiscovery discovery;

    @BeforeEach
    void setUp() throws Exception {
        h = new Harness();
        registry = new PushRegistry();
        proxy = new FakeProxy();
        int port = h.tcp(registry, proxy);
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
    void expiredMarksStaleAndNextGetReopensAndReleasesOld() throws Exception {
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
        assertEquals(List.of("proxy-1"), proxy.released);
        assertTrue(first.isShutdown(), "old channel is shut down");
        assertFalse(second[0].isShutdown());
    }

    @Test
    void registeredAfterExpiredEagerlyRefreshesWithoutGet() throws Exception {
        discovery.get(c -> c, TEMPLATE);
        assertTrue(TestUtil.await(3000, () -> !registry.watchers.isEmpty()));

        registry.push(RegistryEvent.EventType.EXPIRED);
        registry.push(RegistryEvent.EventType.REGISTERED);
        assertTrue(TestUtil.await(5000, () -> proxy.opens.get() == 2), "eager refresh on register");
        assertEquals(List.of("proxy-1"), proxy.released);
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

    /**
     * Desired behavior (not current): a "modified" event should refresh the
     * cached entry without shutting down a channel the caller already holds.
     * Today refresh() closes the old entry's channel out from under callers.
     */
    @Test
    @Disabled("known bug: 'modified' closes the channel a caller is still holding")
    void modifiedEventDoesNotCloseCallersHeldChannel() throws Exception {
        ManagedChannel held = discovery.get(c -> c, TEMPLATE);
        assertTrue(TestUtil.await(3000, () -> !registry.watchers.isEmpty()));
        registry.push(RegistryEvent.EventType.MODIFIED);
        assertTrue(TestUtil.await(5000, () -> proxy.opens.get() == 2));
        assertFalse(held.isShutdown());
    }
}
