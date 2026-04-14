package io.coordin8;

import coordin8.LeaseServiceGrpc;
import coordin8.LeaseOuterClass.GrantRequest;
import coordin8.LeaseOuterClass.RenewRequest;
import coordin8.LeaseOuterClass.CancelRequest;
import coordin8.LeaseOuterClass.Lease;
import io.grpc.ManagedChannel;

import java.io.Closeable;
import java.time.Instant;
import java.util.concurrent.Executors;
import java.util.concurrent.ScheduledExecutorService;
import java.util.concurrent.ScheduledFuture;
import java.util.concurrent.TimeUnit;

public class LeaseClient {

    private final LeaseServiceGrpc.LeaseServiceBlockingStub stub;

    LeaseClient(ManagedChannel channel) {
        this.stub = LeaseServiceGrpc.newBlockingStub(channel);
    }

    public LeaseRecord grant(String resourceId, long ttlSeconds) {
        Lease lease = stub.grant(GrantRequest.newBuilder()
                .setResourceId(resourceId)
                .setTtlSeconds(ttlSeconds)
                .build());
        return toRecord(lease);
    }

    public LeaseRecord renew(String leaseId, long ttlSeconds) {
        Lease lease = stub.renew(RenewRequest.newBuilder()
                .setLeaseId(leaseId)
                .setTtlSeconds(ttlSeconds)
                .build());
        return toRecord(lease);
    }

    public void cancel(String leaseId) {
        stub.cancel(CancelRequest.newBuilder()
                .setLeaseId(leaseId)
                .build());
    }

    /**
     * Start a background thread that renews the given lease at half its TTL interval.
     * Returns a {@link Closeable} — call {@code close()} to stop the renewal loop
     * (this does NOT cancel the lease itself).
     *
     * @param leaseId    the lease to keep alive
     * @param ttlSeconds the TTL to request on each renewal
     * @return a handle to stop the keep-alive loop
     */
    public Closeable keepAlive(String leaseId, long ttlSeconds) {
        ScheduledExecutorService scheduler = Executors.newSingleThreadScheduledExecutor(r -> {
            Thread t = new Thread(r, "lease-keepalive-" + leaseId);
            t.setDaemon(true);
            return t;
        });
        long intervalSeconds = Math.max(ttlSeconds / 2, 1);
        ScheduledFuture<?> future = scheduler.scheduleAtFixedRate(() -> {
            try {
                renew(leaseId, ttlSeconds);
            } catch (Exception e) {
                scheduler.shutdown();
            }
        }, intervalSeconds, intervalSeconds, TimeUnit.SECONDS);

        return () -> {
            future.cancel(false);
            scheduler.shutdown();
        };
    }

    private static LeaseRecord toRecord(Lease l) {
        return new LeaseRecord(
                l.getLeaseId(),
                l.getResourceId(),
                l.hasGrantedAt() ? Instant.ofEpochSecond(l.getGrantedAt().getSeconds()) : null,
                l.hasExpiresAt() ? Instant.ofEpochSecond(l.getExpiresAt().getSeconds()) : null,
                l.getTtlSeconds()
        );
    }

    public record LeaseRecord(
            String leaseId,
            String resourceId,
            Instant grantedAt,
            Instant expiresAt,
            long ttlSeconds
    ) {}
}
