package io.coordin8;

import com.google.protobuf.ByteString;
import coordin8.EventOuterClass;
import coordin8.EventServiceGrpc;
import io.grpc.ManagedChannel;
import io.grpc.stub.StreamObserver;

import java.time.Instant;
import java.util.Map;
import java.util.function.Consumer;

public class EventClient {

    private final EventServiceGrpc.EventServiceBlockingStub stub;
    private final EventServiceGrpc.EventServiceStub asyncStub;

    EventClient(ManagedChannel channel) {
        this.stub = EventServiceGrpc.newBlockingStub(channel);
        this.asyncStub = EventServiceGrpc.newStub(channel);
    }

    /**
     * Subscribe to events from a source matching a template.
     *
     * @param source     event source name (e.g. "market.signals")
     * @param template   attribute template to filter events (null = all)
     * @param durable    true for DURABLE (mailbox), false for BEST_EFFORT
     * @param ttlSeconds subscription lease TTL
     * @param handback   opaque data echoed with events (may be null)
     * @return the event registration with lease info
     */
    public EventRegistrationRecord subscribe(String source, Map<String, String> template,
                                             boolean durable, long ttlSeconds, byte[] handback) {
        EventOuterClass.SubscribeRequest.Builder req = EventOuterClass.SubscribeRequest.newBuilder()
                .setSource(source)
                .setDelivery(durable
                        ? EventOuterClass.DeliveryMode.DURABLE
                        : EventOuterClass.DeliveryMode.BEST_EFFORT)
                .setTtlSeconds(ttlSeconds);
        if (template != null) req.putAllTemplate(template);
        if (handback != null) req.setHandback(ByteString.copyFrom(handback));

        EventOuterClass.EventRegistration resp = stub.subscribe(req.build());
        String leaseId = resp.hasLease() ? resp.getLease().getLeaseId() : null;
        return new EventRegistrationRecord(
                resp.getRegistrationId(), resp.getSource(), leaseId, resp.getSeqNum());
    }

    /**
     * Receive events for a registration (server-streaming).
     * The callback is invoked on a gRPC thread for each event.
     *
     * @param registrationId the subscription registration ID
     * @param onEvent        callback for each event
     * @param onError        callback for errors (stream closed)
     */
    public void receive(String registrationId,
                        Consumer<EventRecord> onEvent,
                        Consumer<Throwable> onError) {
        asyncStub.receive(
                EventOuterClass.ReceiveRequest.newBuilder()
                        .setRegistrationId(registrationId)
                        .build(),
                new StreamObserver<EventOuterClass.Event>() {
                    @Override
                    public void onNext(EventOuterClass.Event evt) {
                        Instant emittedAt = evt.hasEmittedAt()
                                ? Instant.ofEpochSecond(evt.getEmittedAt().getSeconds(),
                                                        evt.getEmittedAt().getNanos())
                                : null;
                        onEvent.accept(new EventRecord(
                                evt.getEventId(),
                                evt.getSource(),
                                evt.getEventType(),
                                evt.getSeqNum(),
                                evt.getAttrsMap(),
                                evt.getPayload().toByteArray(),
                                evt.getHandback().toByteArray(),
                                emittedAt));
                    }

                    @Override
                    public void onError(Throwable t) {
                        onError.accept(t);
                    }

                    @Override
                    public void onCompleted() {}
                });
    }

    /**
     * Emit an event into the bus. Any service can call this.
     *
     * @param source    event source name
     * @param eventType event type identifier
     * @param attrs     event attributes (may be null)
     * @param payload   opaque event body (may be null)
     */
    public void emit(String source, String eventType,
                     Map<String, String> attrs, byte[] payload) {
        EventOuterClass.EmitRequest.Builder req = EventOuterClass.EmitRequest.newBuilder()
                .setSource(source)
                .setEventType(eventType);
        if (attrs != null) req.putAllAttrs(attrs);
        if (payload != null) req.setPayload(ByteString.copyFrom(payload));
        stub.emit(req.build());
    }

    /**
     * Renew a subscription lease.
     */
    public LeaseClient.LeaseRecord renewSubscription(String registrationId, long ttlSeconds) {
        coordin8.LeaseOuterClass.Lease lease = stub.renewSubscription(
                EventOuterClass.RenewSubscriptionRequest.newBuilder()
                        .setRegistrationId(registrationId)
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
     * Cancel a subscription explicitly.
     */
    public void cancelSubscription(String registrationId) {
        stub.cancelSubscription(
                EventOuterClass.CancelSubscriptionRequest.newBuilder()
                        .setRegistrationId(registrationId)
                        .build());
    }

    // ── Records ─────────────────────────────────────────────────────────────

    public record EventRegistrationRecord(
            String registrationId,
            String source,
            String leaseId,
            long seqNum
    ) {}

    public record EventRecord(
            String eventId,
            String source,
            String eventType,
            long seqNum,
            Map<String, String> attrs,
            byte[] payload,
            byte[] handback,
            Instant emittedAt
    ) {}
}
