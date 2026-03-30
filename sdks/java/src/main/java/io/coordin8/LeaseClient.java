package io.coordin8;

import coordin8.LeaseServiceGrpc;
import coordin8.LeaseOuterClass.GrantRequest;
import coordin8.LeaseOuterClass.RenewRequest;
import coordin8.LeaseOuterClass.CancelRequest;
import coordin8.LeaseOuterClass.Lease;
import io.grpc.ManagedChannel;

import java.time.Instant;

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
