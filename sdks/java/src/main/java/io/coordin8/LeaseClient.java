package io.coordin8;

import coordin8.LeaseServiceGrpc;
import coordin8.LeaseOuterClass.GrantRequest;
import coordin8.LeaseOuterClass.RenewRequest;
import coordin8.LeaseOuterClass.CancelRequest;
import coordin8.LeaseOuterClass.Lease;
import io.grpc.ManagedChannel;
import io.grpc.ManagedChannelBuilder;
import io.grpc.Status;
import io.grpc.StatusRuntimeException;

import java.io.Closeable;
import java.time.Instant;
import java.util.concurrent.Executors;
import java.util.concurrent.ScheduledExecutorService;
import java.util.concurrent.ScheduledFuture;
import java.util.concurrent.TimeUnit;
import java.util.function.Consumer;

/**
 * Wraps the generated {@code LeaseService} gRPC client. There is no single
 * "the" LeaseMgr to connect to — Registry, Space, EventMgr, and TransactionMgr
 * each grant their own leases and mount {@code LeaseService} on their own
 * connection. Build one with {@link #wrap(ManagedChannel)} (given a connection
 * you already hold, e.g. {@link DjinnClient#registryLeases()}) or
 * {@link #dial(String)} (given an arbitrary grantor address, typically read
 * off a {@link LeaseRecord}'s {@code grantorHost}/{@code grantorPort}).
 */
public class LeaseClient implements Closeable {

    private final LeaseServiceGrpc.LeaseServiceBlockingStub stub;
    private final ManagedChannel ownedChannel; // null if wrapping a channel we don't own

    private LeaseClient(ManagedChannel channel, ManagedChannel ownedChannel) {
        this.stub = LeaseServiceGrpc.newBlockingStub(channel);
        this.ownedChannel = ownedChannel;
    }

    /**
     * Wrap an existing gRPC connection (e.g. one already held by a
     * {@link DjinnClient}) as a {@link LeaseClient}. Does not take ownership —
     * the caller is still responsible for closing {@code channel}; calling
     * {@link #close()} on the returned client is a no-op.
     */
    public static LeaseClient wrap(ManagedChannel channel) {
        return new LeaseClient(channel, null);
    }

    /**
     * Dial a lease grantor's address directly — typically
     * {@code grantorHost:grantorPort} read off a {@link LeaseRecord} already
     * held. The returned {@link LeaseClient} owns the connection; call
     * {@link #close()} when done.
     */
    public static LeaseClient dial(String grantorAddr) {
        int idx = grantorAddr.lastIndexOf(':');
        if (idx < 0) {
            throw new IllegalArgumentException("grantor address must be host:port, got: " + grantorAddr);
        }
        String host = grantorAddr.substring(0, idx);
        int port = Integer.parseInt(grantorAddr.substring(idx + 1));
        ManagedChannel channel = ManagedChannelBuilder.forAddress(host, port).usePlaintext().build();
        return new LeaseClient(channel, channel);
    }

    /**
     * Release the underlying connection if this client owns one (built via
     * {@link #dial(String)}). A no-op for one built via {@link #wrap}.
     */
    @Override
    public void close() {
        if (ownedChannel != null) {
            ownedChannel.shutdown();
            try {
                ownedChannel.awaitTermination(5, TimeUnit.SECONDS);
            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
            }
        }
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
     * Start a background thread that renews the given lease at half its TTL
     * interval. Returns a {@link Closeable} — call {@code close()} to stop
     * the renewal loop (this does NOT cancel the lease itself).
     *
     * <p>{@code onFailure} is invoked on every failed renewal attempt, so the
     * caller always learns about a lost lease instead of it failing silently.
     * A transient transport error is reported but the loop keeps retrying on
     * the next tick; {@code NOT_FOUND}/{@code FAILED_PRECONDITION} (the
     * resource is genuinely gone — lease expired or never existed) is
     * reported once and stops the loop, mirroring the Go SDK's
     * {@code KeepAlive} semantics.
     *
     * @param leaseId    the lease to keep alive
     * @param ttlSeconds the TTL to request on each renewal
     * @param onFailure  callback invoked with the failure on every failed
     *                   renewal attempt (transient or terminal)
     * @return a handle to stop the keep-alive loop
     */
    public Closeable keepAlive(String leaseId, long ttlSeconds, Consumer<Throwable> onFailure) {
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
                onFailure.accept(e);
                if (e instanceof StatusRuntimeException sre) {
                    Status.Code code = sre.getStatus().getCode();
                    if (code == Status.Code.NOT_FOUND || code == Status.Code.FAILED_PRECONDITION) {
                        // The resource is genuinely gone (lease not found /
                        // expired) — no point retrying.
                        scheduler.shutdown();
                        return;
                    }
                }
                // Transient failure — keep trying on the next tick.
            }
        }, intervalSeconds, intervalSeconds, TimeUnit.SECONDS);

        return () -> {
            future.cancel(false);
            scheduler.shutdown();
        };
    }

    static LeaseRecord toRecord(Lease l) {
        return new LeaseRecord(
                l.getLeaseId(),
                l.getResourceId(),
                l.hasGrantedAt() ? Instant.ofEpochSecond(l.getGrantedAt().getSeconds()) : null,
                l.hasExpiresAt() ? Instant.ofEpochSecond(l.getExpiresAt().getSeconds()) : null,
                l.getTtlSeconds(),
                l.getGrantorHost(),
                l.getGrantorPort()
        );
    }

    public record LeaseRecord(
            String leaseId,
            String resourceId,
            Instant grantedAt,
            Instant expiresAt,
            long ttlSeconds,
            String grantorHost,
            int grantorPort
    ) {
        /** {@code "host:port"} for renewing/cancelling this lease — pass it to {@link LeaseClient#dial(String)}. */
        public String grantorAddr() {
            return grantorHost + ":" + grantorPort;
        }
    }
}
