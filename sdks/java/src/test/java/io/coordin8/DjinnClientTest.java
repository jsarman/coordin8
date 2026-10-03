package io.coordin8;

import coordin8.Registry.*;
import coordin8.RegistryServiceGrpc;
import coordin8.Space;
import coordin8.SpaceServiceGrpc;
import io.coordin8.TestUtil.Harness;
import io.grpc.stub.StreamObserver;
import org.junit.jupiter.api.Test;

import java.util.ArrayList;
import java.util.Collections;
import java.util.List;
import java.util.Map;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.atomic.AtomicInteger;

import static org.junit.jupiter.api.Assertions.*;

class DjinnClientTest {

    /** Registry that answers Lookup from a map keyed by interface name. */
    static class MapRegistry extends RegistryServiceGrpc.RegistryServiceImplBase {
        final Map<String, Capability> caps = new ConcurrentHashMap<>();
        final List<String> lookups = Collections.synchronizedList(new ArrayList<>());

        void put(String iface, String host, String port) {
            TransportDescriptor.Builder t = TransportDescriptor.newBuilder().setType("grpc");
            if (host != null) t.putConfig("host", host);
            if (port != null) t.putConfig("port", port);
            caps.put(iface, Capability.newBuilder().setCapabilityId(iface + "-id").setInterface(iface)
                    .setTransport(t).build());
        }

        @Override
        public void lookup(LookupRequest req, StreamObserver<Capability> out) {
            String iface = req.getTemplateMap().get("interface");
            lookups.add(iface);
            Capability c = caps.get(iface);
            if (c == null) {
                out.onError(io.grpc.Status.NOT_FOUND.asRuntimeException());
                return;
            }
            out.onNext(c);
            out.onCompleted();
        }
    }

    static class CountingSpace extends SpaceServiceGrpc.SpaceServiceImplBase {
        final AtomicInteger writes = new AtomicInteger();

        @Override
        public void write(Space.WriteRequest req, StreamObserver<Space.WriteResponse> out) {
            writes.incrementAndGet();
            out.onNext(Space.WriteResponse.newBuilder()
                    .setTuple(Space.Tuple.newBuilder().setTupleId("t").putAllAttrs(req.getAttrsMap())).build());
            out.onCompleted();
        }
    }

    @Test
    void connectResolvesProxySpaceAndEventThroughRegistry() throws Exception {
        try (Harness h = new Harness()) {
            MapRegistry reg = new MapRegistry();
            CountingSpace space = new CountingSpace();
            int regPort = h.tcp(reg);
            int spacePort = h.tcp(space);
            reg.put("Proxy", "localhost", "1");
            reg.put("Space", "localhost", String.valueOf(spacePort));
            reg.put("EventMgr", "localhost", "2");

            try (DjinnClient djinn = DjinnClient.connect("localhost:" + regPort)) {
                assertEquals(List.of("Proxy", "Space", "EventMgr"), reg.lookups);
                // The Space client really points at the address Registry returned.
                assertEquals("t", djinn.space().write(Map.of("a", "b"), 10).tupleId());
                assertEquals(1, space.writes.get());
            }
        }
    }

    @Test
    void pinnedAddressesSkipRegistryLookup() throws Exception {
        try (Harness h = new Harness()) {
            MapRegistry reg = new MapRegistry();
            CountingSpace space = new CountingSpace();
            int regPort = h.tcp(reg);
            int spacePort = h.tcp(space);
            reg.put("Proxy", "localhost", "1");
            reg.put("EventMgr", "localhost", "2");
            // Space deliberately absent from the registry but pinned.
            try (DjinnClient djinn = DjinnClient.connect("localhost:" + regPort, null,
                    "localhost:" + spacePort, null)) {
                assertEquals(List.of("Proxy", "EventMgr"), reg.lookups);
                djinn.space().write(Map.of(), 5);
                assertEquals(1, space.writes.get());
            }
        }
    }

    @Test
    void connectFailsWhenServiceNotInRegistry() throws Exception {
        try (Harness h = new Harness()) {
            MapRegistry reg = new MapRegistry();
            int regPort = h.tcp(reg);
            reg.put("Proxy", "localhost", "1");
            var ex = assertThrows(IllegalStateException.class,
                    () -> DjinnClient.connect("localhost:" + regPort));
            assertTrue(ex.getMessage().contains("Space"), ex.getMessage());
            assertTrue(ex.getMessage().contains("not found"), ex.getMessage());
        }
    }

    @Test
    void connectFailsWhenTransportMissingHostOrPort() throws Exception {
        try (Harness h = new Harness()) {
            MapRegistry reg = new MapRegistry();
            int regPort = h.tcp(reg);
            reg.put("Proxy", "localhost", null);
            var ex = assertThrows(IllegalStateException.class,
                    () -> DjinnClient.connect("localhost:" + regPort));
            assertTrue(ex.getMessage().contains("missing host/port"), ex.getMessage());
        }
    }

    @Test
    void connectFailsWhenEntryHasNoTransport() throws Exception {
        try (Harness h = new Harness()) {
            MapRegistry reg = new MapRegistry() {
                @Override
                public void lookup(LookupRequest req, StreamObserver<Capability> out) {
                    out.onNext(Capability.newBuilder().setCapabilityId("x").setInterface("Proxy").build());
                    out.onCompleted();
                }
            };
            int regPort = h.tcp(reg);
            var ex = assertThrows(IllegalStateException.class,
                    () -> DjinnClient.connect("localhost:" + regPort));
            assertTrue(ex.getMessage().contains("no transport"), ex.getMessage());
        }
    }

    @Test
    void connectRejectsAddressWithoutPort() {
        assertThrows(IllegalArgumentException.class, () -> DjinnClient.connect("nohost"));
    }

    @Test
    void tokenIsAttachedToEveryConnection() throws Exception {
        AuthTest.HeaderCapture regCap = new AuthTest.HeaderCapture();
        AuthTest.HeaderCapture spaceCap = new AuthTest.HeaderCapture();
        try (Harness h = new Harness()) {
            MapRegistry reg = new MapRegistry();
            CountingSpace space = new CountingSpace();
            int regPort = h.tcp(regCap, reg);
            int spacePort = h.tcp(spaceCap, space);
            reg.put("Proxy", "localhost", "1");
            reg.put("Space", "localhost", String.valueOf(spacePort));
            reg.put("EventMgr", "localhost", "2");

            try (DjinnClient djinn = DjinnClient.connect("localhost:" + regPort, "jwt-abc")) {
                djinn.space().write(Map.of(), 5);
            }
        }
        // 3 bootstrap lookups, all authenticated.
        assertEquals(3, regCap.seen.size());
        assertTrue(regCap.seen.stream().allMatch("Bearer jwt-abc"::equals), regCap.seen.toString());
        assertEquals(List.of("Bearer jwt-abc"), spaceCap.seen);
    }

    @Test
    void noTokenMeansNoAuthorizationHeader() throws Exception {
        AuthTest.HeaderCapture regCap = new AuthTest.HeaderCapture();
        try (Harness h = new Harness()) {
            MapRegistry reg = new MapRegistry();
            int regPort = h.tcp(regCap, reg);
            reg.put("Proxy", "localhost", "1");
            reg.put("Space", "localhost", "2");
            reg.put("EventMgr", "localhost", "3");
            try (DjinnClient djinn = DjinnClient.connect("localhost:" + regPort)) {
                assertNotNull(djinn.registry());
            }
        }
        assertTrue(regCap.seen.stream().allMatch(java.util.Objects::isNull));
    }
}
