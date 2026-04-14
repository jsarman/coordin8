package io.coordin8;

import com.google.protobuf.ByteString;
import coordin8.Space;
import coordin8.SpaceServiceGrpc;
import io.grpc.ManagedChannel;
import io.grpc.StatusRuntimeException;
import io.grpc.stub.StreamObserver;

import java.time.Instant;
import java.util.ArrayList;
import java.util.Iterator;
import java.util.List;
import java.util.Map;
import java.util.Optional;
import java.util.function.Consumer;

public class SpaceClient {

    private final SpaceServiceGrpc.SpaceServiceBlockingStub stub;
    private final SpaceServiceGrpc.SpaceServiceStub asyncStub;

    SpaceClient(ManagedChannel channel) {
        this.stub = SpaceServiceGrpc.newBlockingStub(channel);
        this.asyncStub = SpaceServiceGrpc.newStub(channel);
    }

    // ── Write ───────────────────────────────────────────────────────────────

    /**
     * Write a leased tuple into the Space.
     *
     * @param attrs        indexed fields for template matching
     * @param ttlSeconds   lease TTL in seconds
     * @return the written tuple record
     */
    public TupleRecord write(Map<String, String> attrs, long ttlSeconds) {
        return write(attrs, null, ttlSeconds, null, null, null);
    }

    /**
     * Write a leased tuple into the Space with full options.
     *
     * @param attrs        indexed fields for template matching
     * @param payload      opaque data (may be null)
     * @param ttlSeconds   lease TTL in seconds
     * @param writtenBy    writer identity for provenance (may be null)
     * @param inputTupleId lineage link to a predecessor tuple (may be null)
     * @param txnId        transaction ID (may be null)
     * @return the written tuple record
     */
    public TupleRecord write(Map<String, String> attrs, byte[] payload,
                             long ttlSeconds, String writtenBy,
                             String inputTupleId, String txnId) {
        Space.WriteRequest.Builder req = Space.WriteRequest.newBuilder()
                .setTtlSeconds(ttlSeconds);
        if (attrs != null) req.putAllAttrs(attrs);
        if (payload != null) req.setPayload(ByteString.copyFrom(payload));
        if (writtenBy != null) req.setWrittenBy(writtenBy);
        if (inputTupleId != null) req.setInputTupleId(inputTupleId);
        if (txnId != null) req.setTxnId(txnId);

        Space.WriteResponse resp = stub.write(req.build());
        return toRecord(resp.getTuple());
    }

    // ── Read ────────────────────────────────────────────────────────────────

    /**
     * Non-destructive read by template. Returns immediately if no match.
     */
    public Optional<TupleRecord> read(Map<String, String> template) {
        return read(template, false, 0, null);
    }

    /**
     * Non-destructive read by template.
     *
     * @param template   attribute pattern to match
     * @param wait       true to block until a match arrives
     * @param timeoutMs  max wait in milliseconds (0 + wait=true = forever)
     * @param txnId      transaction ID (may be null)
     */
    public Optional<TupleRecord> read(Map<String, String> template, boolean wait,
                                      long timeoutMs, String txnId) {
        Space.ReadRequest.Builder req = Space.ReadRequest.newBuilder()
                .setWait(wait)
                .setTimeoutMs(timeoutMs);
        if (template != null) req.putAllTemplate(template);
        if (txnId != null) req.setTxnId(txnId);

        Space.ReadResponse resp = stub.read(req.build());
        if (!resp.hasTuple()) return Optional.empty();
        return Optional.of(toRecord(resp.getTuple()));
    }

    // ── Take ────────────────────────────────────────────────────────────────

    /**
     * Atomically claim and remove a matching tuple. Returns immediately if no match.
     */
    public Optional<TupleRecord> take(Map<String, String> template) {
        return take(template, false, 0, null);
    }

    /**
     * Atomically claim and remove a matching tuple.
     *
     * @param template   attribute pattern to match
     * @param wait       true to block until a match arrives
     * @param timeoutMs  max wait in milliseconds (0 + wait=true = forever)
     * @param txnId      transaction ID (may be null)
     */
    public Optional<TupleRecord> take(Map<String, String> template, boolean wait,
                                      long timeoutMs, String txnId) {
        Space.TakeRequest.Builder req = Space.TakeRequest.newBuilder()
                .setWait(wait)
                .setTimeoutMs(timeoutMs);
        if (template != null) req.putAllTemplate(template);
        if (txnId != null) req.setTxnId(txnId);

        Space.TakeResponse resp = stub.take(req.build());
        if (!resp.hasTuple()) return Optional.empty();
        return Optional.of(toRecord(resp.getTuple()));
    }

    // ── Contents ────────────────────────────────────────────────────────────

    /**
     * Bulk read — returns all tuples matching the template without removing them.
     */
    public List<TupleRecord> contents(Map<String, String> template) {
        return contents(template, null);
    }

    /**
     * Bulk read — returns all tuples matching the template without removing them.
     *
     * @param template attribute pattern (null or empty = match all)
     * @param txnId    transaction ID (may be null)
     */
    public List<TupleRecord> contents(Map<String, String> template, String txnId) {
        Space.ContentsRequest.Builder req = Space.ContentsRequest.newBuilder();
        if (template != null) req.putAllTemplate(template);
        if (txnId != null) req.setTxnId(txnId);

        Iterator<Space.Tuple> it = stub.contents(req.build());
        List<TupleRecord> results = new ArrayList<>();
        it.forEachRemaining(t -> results.add(toRecord(t)));
        return results;
    }

    // ── Notify ──────────────────────────────────────────────────────────────

    /**
     * Watch for tuple events matching the template.
     *
     * @param template   attribute pattern to watch
     * @param on         event type: "appearance" or "expiration"
     * @param ttlSeconds lease TTL for the watch itself
     * @param handback   opaque data echoed with events (may be null)
     * @param onEvent    callback for each event (invoked on a gRPC thread)
     * @param onError    callback for errors (stream closed)
     */
    public void notify(Map<String, String> template, String on,
                       long ttlSeconds, byte[] handback,
                       Consumer<SpaceEventRecord> onEvent,
                       Consumer<Throwable> onError) {
        Space.SpaceEventType eventType = "expiration".equalsIgnoreCase(on)
                ? Space.SpaceEventType.EXPIRATION
                : Space.SpaceEventType.APPEARANCE;

        Space.NotifyRequest.Builder req = Space.NotifyRequest.newBuilder()
                .setOn(eventType)
                .setTtlSeconds(ttlSeconds);
        if (template != null) req.putAllTemplate(template);
        if (handback != null) req.setHandback(ByteString.copyFrom(handback));

        asyncStub.notify(req.build(), new StreamObserver<Space.SpaceEvent>() {
            @Override
            public void onNext(Space.SpaceEvent evt) {
                TupleRecord tuple = evt.hasTuple() ? toRecord(evt.getTuple()) : null;
                Instant occurredAt = evt.hasOccurredAt()
                        ? Instant.ofEpochSecond(evt.getOccurredAt().getSeconds(),
                                                evt.getOccurredAt().getNanos())
                        : null;
                String type = evt.getType() == Space.SpaceEventType.EXPIRATION
                        ? "expiration" : "appearance";
                onEvent.accept(new SpaceEventRecord(
                        type, tuple, occurredAt, evt.getHandback().toByteArray()));
            }

            @Override
            public void onError(Throwable t) {
                onError.accept(t);
            }

            @Override
            public void onCompleted() {}
        });
    }

    // ── Renew / Cancel ──────────────────────────────────────────────────────

    /**
     * Renew a tuple's lease.
     */
    public LeaseClient.LeaseRecord renewTuple(String tupleId, long ttlSeconds) {
        coordin8.LeaseOuterClass.Lease lease = stub.renew(
                Space.RenewTupleRequest.newBuilder()
                        .setTupleId(tupleId)
                        .setTtlSeconds(ttlSeconds)
                        .build());
        return new LeaseClient.LeaseRecord(
                lease.getLeaseId(),
                lease.getResourceId(),
                lease.hasGrantedAt() ? Instant.ofEpochSecond(lease.getGrantedAt().getSeconds()) : null,
                lease.hasExpiresAt() ? Instant.ofEpochSecond(lease.getExpiresAt().getSeconds()) : null,
                lease.getTtlSeconds());
    }

    /**
     * Cancel (remove) a tuple and its lease.
     */
    public void cancelTuple(String tupleId) {
        stub.cancel(Space.CancelTupleRequest.newBuilder()
                .setTupleId(tupleId)
                .build());
    }

    // ── Records ─────────────────────────────────────────────────────────────

    public record TupleRecord(
            String tupleId,
            Map<String, String> attrs,
            byte[] payload,
            String leaseId,
            String writtenBy,
            Instant writtenAt,
            String inputTupleId
    ) {}

    public record SpaceEventRecord(
            String type,
            TupleRecord tuple,
            Instant occurredAt,
            byte[] handback
    ) {}

    // ── Helpers ─────────────────────────────────────────────────────────────

    private static TupleRecord toRecord(Space.Tuple t) {
        String leaseId = t.hasLease() ? t.getLease().getLeaseId() : null;
        String writtenBy = null;
        Instant writtenAt = null;
        String inputTupleId = null;
        if (t.hasProvenance()) {
            writtenBy = t.getProvenance().getWrittenBy();
            inputTupleId = t.getProvenance().getInputTupleId();
            if (t.getProvenance().hasWrittenAt()) {
                writtenAt = Instant.ofEpochSecond(
                        t.getProvenance().getWrittenAt().getSeconds(),
                        t.getProvenance().getWrittenAt().getNanos());
            }
        }
        return new TupleRecord(
                t.getTupleId(),
                t.getAttrsMap(),
                t.getPayload().toByteArray(),
                leaseId,
                writtenBy,
                writtenAt,
                inputTupleId);
    }
}
