package coordin8

import (
	"context"
	"time"

	pb "github.com/coordin8/sdk-go/gen/coordin8"
	"google.golang.org/grpc"
)

// LeaseClient wraps the generated LeaseService gRPC client.
type LeaseClient struct {
	client pb.LeaseServiceClient
}

// Leases returns a client for LeaseMgr operations.
func (c *Client) Leases() *LeaseClient {
	return &LeaseClient{client: pb.NewLeaseServiceClient(c.leaseConn)}
}

// LeaseRecord is a live lease returned from the Djinn.
type LeaseRecord struct {
	LeaseID    string
	ResourceID string
	GrantedAt  time.Time
	ExpiresAt  time.Time
}

func protoToRecord(l *pb.Lease) LeaseRecord {
	r := LeaseRecord{
		LeaseID:    l.LeaseId,
		ResourceID: l.ResourceId,
	}
	if l.GrantedAt != nil {
		r.GrantedAt = l.GrantedAt.AsTime()
	}
	if l.ExpiresAt != nil {
		r.ExpiresAt = l.ExpiresAt.AsTime()
	}
	return r
}

// Grant requests a new lease for resourceID with the given TTL.
func (c *LeaseClient) Grant(ctx context.Context, resourceID string, ttl time.Duration) (LeaseRecord, error) {
	resp, err := c.client.Grant(ctx, &pb.GrantRequest{
		ResourceId: resourceID,
		TtlSeconds: uint64(ttl.Seconds()),
	})
	if err != nil {
		return LeaseRecord{}, err
	}
	return protoToRecord(resp), nil
}

// Renew extends an existing lease.
func (c *LeaseClient) Renew(ctx context.Context, leaseID string, ttl time.Duration) (LeaseRecord, error) {
	resp, err := c.client.Renew(ctx, &pb.RenewRequest{
		LeaseId:    leaseID,
		TtlSeconds: uint64(ttl.Seconds()),
	})
	if err != nil {
		return LeaseRecord{}, err
	}
	return protoToRecord(resp), nil
}

// Cancel explicitly terminates a lease.
func (c *LeaseClient) Cancel(ctx context.Context, leaseID string) error {
	_, err := c.client.Cancel(ctx, &pb.CancelRequest{LeaseId: leaseID})
	return err
}

// KeepAlive renews leaseID in the background at half the TTL interval.
// Runs until ctx is cancelled or a renewal fails (lease gone or expired).
func (c *LeaseClient) KeepAlive(ctx context.Context, leaseID string, ttl time.Duration) {
	ticker := time.NewTicker(ttl / 2)
	defer ticker.Stop()
	for {
		select {
		case <-ticker.C:
			if _, err := c.Renew(ctx, leaseID, ttl); err != nil {
				return
			}
		case <-ctx.Done():
			return
		}
	}
}

// ExpiryEvent is delivered when a watched lease expires.
type ExpiryEvent struct {
	LeaseID    string
	ResourceID string
	ExpiredAt  time.Time
}

// Watch streams expiry events for resourceID (empty = all expirations).
// Returns a channel that receives events until ctx is cancelled.
func (c *LeaseClient) Watch(ctx context.Context, resourceID string) (<-chan ExpiryEvent, error) {
	stream, err := c.client.WatchExpiry(ctx, &pb.WatchExpiryRequest{ResourceId: resourceID}, grpc.WaitForReady(true))
	if err != nil {
		return nil, err
	}

	ch := make(chan ExpiryEvent, 16)
	go func() {
		defer close(ch)
		for {
			evt, err := stream.Recv()
			if err != nil {
				return
			}
			e := ExpiryEvent{
				LeaseID:    evt.LeaseId,
				ResourceID: evt.ResourceId,
			}
			if evt.ExpiredAt != nil {
				e.ExpiredAt = evt.ExpiredAt.AsTime()
			}
			select {
			case ch <- e:
			case <-ctx.Done():
				return
			}
		}
	}()
	return ch, nil
}
