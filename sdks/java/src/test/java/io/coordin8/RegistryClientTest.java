package io.coordin8;

import coordin8.Registry.*;
import coordin8.LeaseOuterClass.Lease;
import coordin8.RegistryServiceGrpc;
import io.coordin8.RegistryClient.CapabilityRecord;
import io.coordin8.RegistryClient.RegisterResult;
import io.coordin8.RegistryClient.RegistryEventRecord;
import io.coordin8.TestUtil.Harness;
import io.grpc.Status;
import io.grpc.StatusRuntimeException;
import io.grpc.stub.StreamObserver;
import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.BeforeEach;
import org.junit.jupiter.api.Test;

import java.util.List;
import java.util.Map;
import java.util.Optional;
import java.util.concurrent.CopyOnWriteArrayList;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;

import static org.junit.jupiter.api.Assertions.*;

class RegistryClientTest {

    static Capability cap(String id, String iface, Map<String, String> attrs) {
        return Capability.newBuilder().setCapabilityId(id).setInterface(iface).putAllAttrs(attrs)
                .setTransport(TransportDescriptor.newBuilder().setType("grpc")
                        .putConfig("host", "h").putConfig("port", "1").build())
                .build();
    }

    static class FakeRegistry extends RegistryServiceGrpc.RegistryServiceImplBase {
        volatile RegisterRequest lastRegister;
        volatile ModifyAttrsRequest lastModify;
        volatile LookupRequest lastLookup;
        volatile RegistryWatchRequest lastWatch;
        volatile List<Capability> all = List.of();
        volatile List<RegistryEvent> events = List.of();
        volatile Status watchEndsWith = null; // null = complete normally
        volatile boolean registerWithoutLease = false;

        @Override
        public void register(RegisterRequest req, StreamObserver<RegisterResponse> out) {
            lastRegister = req;
            RegisterResponse.Builder b = RegisterResponse.newBuilder()
                    .setCapabilityId(req.getCapabilityId().isEmpty() ? "cap-new" : req.getCapabilityId());
            if (!registerWithoutLease) {
                b.setLease(Lease.newBuilder().setLeaseId("lease-1").setTtlSeconds(req.getTtlSeconds() / 2));
            }
            out.onNext(b.build());
            out.onCompleted();
        }

        @Override
        public void modifyAttrs(ModifyAttrsRequest req, StreamObserver<Capability> out) {
            lastModify = req;
            out.onNext(cap(req.getCapabilityId(), "Svc", Map.of("modified", "yes")));
            out.onCompleted();
        }

        @Override
        public void lookup(LookupRequest req, StreamObserver<Capability> out) {
            lastLookup = req;
            Optional<Capability> first = all.stream().findFirst();
            if (req.getTemplateMap().containsKey("boom")) {
                out.onError(Status.INTERNAL.asRuntimeException());
            } else if (first.isEmpty()) {
                out.onError(Status.NOT_FOUND.asRuntimeException());
            } else {
                out.onNext(first.get());
                out.onCompleted();
            }
        }

        @Override
        public void lookupAll(LookupRequest req, StreamObserver<Capability> out) {
            lastLookup = req;
            all.forEach(out::onNext);
            out.onCompleted();
        }

        @Override
        public void watch(RegistryWatchRequest req, StreamObserver<RegistryEvent> out) {
            lastWatch = req;
            events.forEach(out::onNext);
            if (watchEndsWith == null) out.onCompleted();
            else out.onError(watchEndsWith.asRuntimeException());
        }
    }

    Harness h;
    FakeRegistry fake;
    RegistryClient client;

    @BeforeEach
    void setUp() throws Exception {
        h = new Harness();
        fake = new FakeRegistry();
        client = new RegistryClient(h.inProcess(fake));
    }

    @AfterEach
    void tearDown() throws Exception {
        h.close();
    }

    @Test
    void registerSendsAllFieldsAndMapsResult() {
        var transport = new RegistryClient.TransportDescriptor("grpc", Map.of("host", "x", "port", "9"));
        RegisterResult r = client.register("Greeter", Map.of("a", "b"), 20, transport, "cap-7", "lease-7");
        RegisterRequest req = fake.lastRegister;
        assertEquals("Greeter", req.getInterface());
        assertEquals(Map.of("a", "b"), req.getAttrsMap());
        assertEquals(20, req.getTtlSeconds());
        assertEquals("grpc", req.getTransport().getType());
        assertEquals(Map.of("host", "x", "port", "9"), req.getTransport().getConfigMap());
        assertEquals("cap-7", req.getCapabilityId());
        assertEquals("lease-7", req.getLeaseId()); // ownership proof for re-registration
        assertEquals("cap-7", r.capabilityId());
        assertEquals("lease-1", r.leaseId());
        assertEquals(10, r.grantedTtlSeconds());
    }

    @Test
    void registerOmitsOptionalFieldsWhenNull() {
        RegisterResult r = client.register("Greeter", null, 10, null);
        RegisterRequest req = fake.lastRegister;
        assertFalse(req.hasTransport());
        assertEquals("", req.getCapabilityId());
        assertEquals("", req.getLeaseId());
        assertTrue(req.getAttrsMap().isEmpty());
        assertEquals("cap-new", r.capabilityId());
    }

    @Test
    void registerWithoutLeaseYieldsNullLeaseAndZeroTtl() {
        fake.registerWithoutLease = true;
        RegisterResult r = client.register("G", Map.of(), 10, null);
        assertNull(r.leaseId());
        assertEquals(0, r.grantedTtlSeconds());
    }

    @Test
    void modifyAttrsSendsAddAndRemove() {
        CapabilityRecord rec = client.modifyAttrs("c1", Map.of("k", "v"), List.of("gone"), "lease-c1");
        assertEquals("c1", fake.lastModify.getCapabilityId());
        assertEquals("lease-c1", fake.lastModify.getLeaseId()); // ownership proof
        assertEquals(Map.of("k", "v"), fake.lastModify.getAddAttrsMap());
        assertEquals(List.of("gone"), fake.lastModify.getRemoveAttrsList());
        assertEquals("c1", rec.capabilityId());
        assertEquals(Map.of("modified", "yes"), rec.attrs());
    }

    @Test
    void modifyAttrsAcceptsNulls() {
        client.modifyAttrs("c1", null, null, null);
        assertTrue(fake.lastModify.getAddAttrsMap().isEmpty());
        assertEquals(0, fake.lastModify.getRemoveAttrsCount());
    }

    @Test
    void lookupMapsCapabilityIncludingTransport() {
        fake.all = List.of(cap("c1", "Greeter", Map.of("lang", "en")));
        Optional<CapabilityRecord> r = client.lookup(Map.of("interface", "Greeter"));
        assertEquals(Map.of("interface", "Greeter"), fake.lastLookup.getTemplateMap());
        assertTrue(r.isPresent());
        assertEquals("c1", r.get().capabilityId());
        assertEquals("Greeter", r.get().interfaceName());
        assertEquals(Map.of("lang", "en"), r.get().attrs());
        assertEquals("grpc", r.get().transport().type());
        assertEquals("h", r.get().transport().config().get("host"));
    }

    @Test
    void lookupReturnsEmptyOnNotFoundButRethrowsOtherErrors() {
        assertTrue(client.lookup(Map.of("interface", "None")).isEmpty());
        var ex = assertThrows(StatusRuntimeException.class, () -> client.lookup(Map.of("boom", "1")));
        assertEquals(Status.Code.INTERNAL, ex.getStatus().getCode());
    }

    @Test
    void capabilityWithoutTransportMapsToNullTransport() {
        var rec = RegistryClient.toRecord(Capability.newBuilder().setCapabilityId("x").setInterface("I").build());
        assertNull(rec.transport());
    }

    @Test
    void lookupAllCollectsStream() {
        fake.all = List.of(cap("a", "G", Map.of()), cap("b", "G", Map.of()), cap("c", "G", Map.of()));
        List<CapabilityRecord> r = client.lookupAll(Map.of("interface", "G"));
        assertEquals(List.of("a", "b", "c"), r.stream().map(CapabilityRecord::capabilityId).toList());
    }

    @Test
    void lookupAllEmptyWhenNoMatches() {
        assertTrue(client.lookupAll(Map.of("interface", "none")).isEmpty());
    }

    @Test
    void watchMapsEventTypes() throws Exception {
        fake.events = List.of(
                RegistryEvent.newBuilder().setType(RegistryEvent.EventType.REGISTERED)
                        .setCapability(cap("a", "G", Map.of())).build(),
                RegistryEvent.newBuilder().setType(RegistryEvent.EventType.EXPIRED)
                        .setCapability(cap("b", "G", Map.of())).build(),
                RegistryEvent.newBuilder().setType(RegistryEvent.EventType.MODIFIED)
                        .setCapability(cap("c", "G", Map.of())).build(),
                RegistryEvent.newBuilder().setType(RegistryEvent.EventType.EXPIRED).build()); // no capability
        List<RegistryEventRecord> got = new CopyOnWriteArrayList<>();
        CountDownLatch latch = new CountDownLatch(4);
        client.watch(Map.of("interface", "G"), e -> { got.add(e); latch.countDown(); }, t -> {});
        assertTrue(latch.await(5, TimeUnit.SECONDS));
        assertEquals(Map.of("interface", "G"), fake.lastWatch.getTemplateMap());
        assertEquals(List.of("registered", "expired", "modified", "expired"),
                got.stream().map(RegistryEventRecord::type).toList());
        assertEquals("a", got.get(0).capability().capabilityId());
        assertNull(got.get(3).capability());
    }

    @Test
    void watchReportsStreamErrors() throws Exception {
        fake.watchEndsWith = Status.UNAVAILABLE;
        CountDownLatch latch = new CountDownLatch(1);
        List<Throwable> errs = new CopyOnWriteArrayList<>();
        client.watch(Map.of(), e -> {}, t -> { errs.add(t); latch.countDown(); });
        assertTrue(latch.await(5, TimeUnit.SECONDS));
        assertEquals(Status.Code.UNAVAILABLE, Status.fromThrowable(errs.get(0)).getCode());
    }
}
