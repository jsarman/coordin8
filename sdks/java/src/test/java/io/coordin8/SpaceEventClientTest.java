package io.coordin8;

import com.google.protobuf.ByteString;
import com.google.protobuf.Empty;
import com.google.protobuf.Timestamp;
import coordin8.EventOuterClass;
import coordin8.EventServiceGrpc;
import coordin8.LeaseOuterClass.Lease;
import coordin8.Space;
import coordin8.SpaceServiceGrpc;
import io.coordin8.TestUtil.Harness;
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

class SpaceEventClientTest {

    static Space.Tuple tuple(String id) {
        return Space.Tuple.newBuilder().setTupleId(id).putAttrs("k", "v")
                .setPayload(ByteString.copyFromUtf8("pay"))
                .setLease(Lease.newBuilder().setLeaseId("lease-" + id))
                .setProvenance(Space.Provenance.newBuilder().setWrittenBy("me").setInputTupleId("in")
                        .setWrittenAt(Timestamp.newBuilder().setSeconds(50).setNanos(7)))
                .build();
    }

    static class FakeSpace extends SpaceServiceGrpc.SpaceServiceImplBase {
        volatile Space.WriteRequest write;
        volatile Space.ReadRequest read;
        volatile Space.TakeRequest take;
        volatile Space.ContentsRequest contents;
        volatile Space.NotifyRequest notify;
        volatile boolean hasMatch = true;

        @Override
        public void write(Space.WriteRequest req, StreamObserver<Space.WriteResponse> out) {
            write = req;
            out.onNext(Space.WriteResponse.newBuilder().setTuple(tuple("w")).build());
            out.onCompleted();
        }

        @Override
        public void read(Space.ReadRequest req, StreamObserver<Space.ReadResponse> out) {
            read = req;
            Space.ReadResponse.Builder b = Space.ReadResponse.newBuilder();
            if (hasMatch) b.setTuple(tuple("r"));
            out.onNext(b.build());
            out.onCompleted();
        }

        @Override
        public void take(Space.TakeRequest req, StreamObserver<Space.TakeResponse> out) {
            take = req;
            Space.TakeResponse.Builder b = Space.TakeResponse.newBuilder();
            if (hasMatch) b.setTuple(tuple("t"));
            out.onNext(b.build());
            out.onCompleted();
        }

        @Override
        public void contents(Space.ContentsRequest req, StreamObserver<Space.Tuple> out) {
            contents = req;
            out.onNext(tuple("c1"));
            out.onNext(tuple("c2"));
            out.onCompleted();
        }

        @Override
        public void notify(Space.NotifyRequest req, StreamObserver<Space.SpaceEvent> out) {
            notify = req;
            out.onNext(Space.SpaceEvent.newBuilder().setType(Space.SpaceEventType.EXPIRATION)
                    .setTuple(tuple("n")).setHandback(req.getHandback())
                    .setOccurredAt(Timestamp.newBuilder().setSeconds(9).setNanos(1)).build());
            out.onNext(Space.SpaceEvent.newBuilder().setType(Space.SpaceEventType.APPEARANCE).build());
            out.onCompleted();
        }

        @Override
        public void renew(Space.RenewTupleRequest req, StreamObserver<Lease> out) {
            out.onNext(Lease.newBuilder().setLeaseId("renewed-" + req.getTupleId())
                    .setTtlSeconds(req.getTtlSeconds()).build());
            out.onCompleted();
        }

        volatile String cancelled;

        @Override
        public void cancel(Space.CancelTupleRequest req, StreamObserver<Empty> out) {
            cancelled = req.getTupleId();
            out.onNext(Empty.getDefaultInstance());
            out.onCompleted();
        }
    }

    static class FakeEvents extends EventServiceGrpc.EventServiceImplBase {
        volatile EventOuterClass.SubscribeRequest subscribe;
        volatile EventOuterClass.EmitRequest emit;
        volatile String cancelled;

        @Override
        public void subscribe(EventOuterClass.SubscribeRequest req, StreamObserver<EventOuterClass.EventRegistration> out) {
            subscribe = req;
            out.onNext(EventOuterClass.EventRegistration.newBuilder().setRegistrationId("reg-1")
                    .setSource(req.getSource()).setSeqNum(42)
                    .setLease(Lease.newBuilder().setLeaseId("sub-lease")).build());
            out.onCompleted();
        }

        @Override
        public void receive(EventOuterClass.ReceiveRequest req, StreamObserver<EventOuterClass.Event> out) {
            out.onNext(EventOuterClass.Event.newBuilder().setEventId("e1").setSource("src").setEventType("tick")
                    .setSeqNum(3).putAttrs("a", "b").setPayload(ByteString.copyFromUtf8("p"))
                    .setHandback(ByteString.copyFromUtf8("hb"))
                    .setEmittedAt(Timestamp.newBuilder().setSeconds(5)).build());
            out.onNext(EventOuterClass.Event.newBuilder().setEventId("e2").build());
            out.onCompleted();
        }

        @Override
        public void emit(EventOuterClass.EmitRequest req, StreamObserver<Empty> out) {
            emit = req;
            out.onNext(Empty.getDefaultInstance());
            out.onCompleted();
        }

        @Override
        public void renewSubscription(EventOuterClass.RenewSubscriptionRequest req, StreamObserver<Lease> out) {
            out.onNext(Lease.newBuilder().setLeaseId(req.getRegistrationId()).setTtlSeconds(req.getTtlSeconds()).build());
            out.onCompleted();
        }

        @Override
        public void cancelSubscription(EventOuterClass.CancelSubscriptionRequest req, StreamObserver<Empty> out) {
            cancelled = req.getRegistrationId();
            out.onNext(Empty.getDefaultInstance());
            out.onCompleted();
        }
    }

    Harness h;
    FakeSpace fakeSpace;
    FakeEvents fakeEvents;
    SpaceClient space;
    EventClient events;

    @BeforeEach
    void setUp() throws Exception {
        h = new Harness();
        fakeSpace = new FakeSpace();
        fakeEvents = new FakeEvents();
        space = new SpaceClient(h.inProcess(fakeSpace));
        events = new EventClient(h.inProcess(fakeEvents));
    }

    @AfterEach
    void tearDown() throws Exception {
        h.close();
    }

    // ── Space ───────────────────────────────────────────────────────────────

    @Test
    void writeFullOptionsAndTupleMapping() {
        var t = space.write(Map.of("a", "b"), "data".getBytes(), 30, "writer", "pred", "txn-1");
        var req = fakeSpace.write;
        assertEquals(Map.of("a", "b"), req.getAttrsMap());
        assertEquals("data", req.getPayload().toStringUtf8());
        assertEquals(30, req.getTtlSeconds());
        assertEquals("writer", req.getWrittenBy());
        assertEquals("pred", req.getInputTupleId());
        assertEquals("txn-1", req.getTxnId());

        assertEquals("w", t.tupleId());
        assertEquals("lease-w", t.leaseId());
        assertEquals("me", t.writtenBy());
        assertEquals("in", t.inputTupleId());
        assertEquals(50, t.writtenAt().getEpochSecond());
        assertEquals(7, t.writtenAt().getNano());
        assertEquals("pay", new String(t.payload()));
        assertEquals(Map.of("k", "v"), t.attrs());
    }

    @Test
    void simpleWriteOmitsOptionalFields() {
        space.write(Map.of("a", "b"), 10);
        var req = fakeSpace.write;
        assertTrue(req.getPayload().isEmpty());
        assertEquals("", req.getWrittenBy());
        assertEquals("", req.getTxnId());
    }

    @Test
    void readAndTakeReturnEmptyWhenNoTuple() {
        fakeSpace.hasMatch = false;
        assertEquals(Optional.empty(), space.read(Map.of("k", "v")));
        assertEquals(Optional.empty(), space.take(Map.of("k", "v")));
    }

    @Test
    void readAndTakePassWaitTimeoutAndTxn() {
        Optional<SpaceClient.TupleRecord> r = space.read(Map.of("k", "v"), true, 1500, "tx");
        assertEquals("r", r.orElseThrow().tupleId());
        assertTrue(fakeSpace.read.getWait());
        assertEquals(1500, fakeSpace.read.getTimeoutMs());
        assertEquals("tx", fakeSpace.read.getTxnId());
        assertEquals(Map.of("k", "v"), fakeSpace.read.getTemplateMap());

        assertEquals("t", space.take(Map.of(), true, 0, "tx2").orElseThrow().tupleId());
        assertTrue(fakeSpace.take.getWait());
        assertEquals("tx2", fakeSpace.take.getTxnId());

        space.read(Map.of("k", "v"));
        assertFalse(fakeSpace.read.getWait());
        assertEquals("", fakeSpace.read.getTxnId());
    }

    @Test
    void contentsCollectsStreamAndAcceptsNullTemplate() {
        var all = space.contents(null, "tx");
        assertEquals(List.of("c1", "c2"), all.stream().map(SpaceClient.TupleRecord::tupleId).toList());
        assertEquals("tx", fakeSpace.contents.getTxnId());
        assertTrue(fakeSpace.contents.getTemplateMap().isEmpty());
    }

    @Test
    void notifyMapsEventTypesAndHandback() throws Exception {
        List<SpaceClient.SpaceEventRecord> got = new CopyOnWriteArrayList<>();
        CountDownLatch latch = new CountDownLatch(2);
        space.notify(Map.of("k", "v"), "expiration", 60, "hb".getBytes(),
                e -> { got.add(e); latch.countDown(); }, t -> {});
        assertTrue(latch.await(5, TimeUnit.SECONDS));
        assertEquals(Space.SpaceEventType.EXPIRATION, fakeSpace.notify.getOn());
        assertEquals(60, fakeSpace.notify.getTtlSeconds());
        assertEquals("expiration", got.get(0).type());
        assertEquals("n", got.get(0).tuple().tupleId());
        assertEquals("hb", new String(got.get(0).handback()));
        assertEquals(9, got.get(0).occurredAt().getEpochSecond());
        assertEquals("appearance", got.get(1).type());
        assertNull(got.get(1).tuple());
        assertNull(got.get(1).occurredAt());
    }

    @Test
    void notifyDefaultsToAppearanceForUnknownOnValue() throws Exception {
        CountDownLatch latch = new CountDownLatch(1);
        space.notify(Map.of(), "whatever", 5, null, e -> latch.countDown(), t -> {});
        assertTrue(latch.await(5, TimeUnit.SECONDS));
        assertEquals(Space.SpaceEventType.APPEARANCE, fakeSpace.notify.getOn());
    }

    @Test
    void renewAndCancelTuple() {
        var lease = space.renewTuple("tid", 12);
        assertEquals("renewed-tid", lease.leaseId());
        assertEquals(12, lease.ttlSeconds());
        space.cancelTuple("tid");
        assertEquals("tid", fakeSpace.cancelled);
    }

    // ── Events ──────────────────────────────────────────────────────────────

    @Test
    void subscribeMapsDeliveryModeAndRegistration() {
        var reg = events.subscribe("src", Map.of("a", "b"), true, 30, "hb".getBytes());
        assertEquals(EventOuterClass.DeliveryMode.DURABLE, fakeEvents.subscribe.getDelivery());
        assertEquals(Map.of("a", "b"), fakeEvents.subscribe.getTemplateMap());
        assertEquals("hb", fakeEvents.subscribe.getHandback().toStringUtf8());
        assertEquals("reg-1", reg.registrationId());
        assertEquals("sub-lease", reg.leaseId());
        assertEquals(42, reg.seqNum());

        events.subscribe("src", null, false, 30, null);
        assertEquals(EventOuterClass.DeliveryMode.BEST_EFFORT, fakeEvents.subscribe.getDelivery());
        assertTrue(fakeEvents.subscribe.getHandback().isEmpty());
    }

    @Test
    void receiveMapsEvents() throws Exception {
        List<EventClient.EventRecord> got = new CopyOnWriteArrayList<>();
        CountDownLatch latch = new CountDownLatch(2);
        events.receive("reg-1", e -> { got.add(e); latch.countDown(); }, t -> {});
        assertTrue(latch.await(5, TimeUnit.SECONDS));
        var e = got.get(0);
        assertEquals("e1", e.eventId());
        assertEquals("tick", e.eventType());
        assertEquals(3, e.seqNum());
        assertEquals(Map.of("a", "b"), e.attrs());
        assertEquals("p", new String(e.payload()));
        assertEquals("hb", new String(e.handback()));
        assertEquals(5, e.emittedAt().getEpochSecond());
        assertNull(got.get(1).emittedAt());
    }

    @Test
    void emitRenewAndCancelSubscription() {
        events.emit("src", "tick", Map.of("a", "b"), "x".getBytes());
        assertEquals("src", fakeEvents.emit.getSource());
        assertEquals("tick", fakeEvents.emit.getEventType());
        assertEquals("x", fakeEvents.emit.getPayload().toStringUtf8());
        events.emit("src", "tick", null, null);
        assertTrue(fakeEvents.emit.getPayload().isEmpty());

        var lease = events.renewSubscription("reg-1", 25);
        assertEquals("reg-1", lease.leaseId());
        assertEquals(25, lease.ttlSeconds());
        events.cancelSubscription("reg-1");
        assertEquals("reg-1", fakeEvents.cancelled);
    }
}
