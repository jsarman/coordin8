package coordin8

import (
	"context"
	"fmt"
	"time"

	pb "github.com/coordin8/sdk-go/gen/coordin8"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/credentials/insecure"
	"google.golang.org/grpc/status"
)

// LeaseClient wraps the generated LeaseService gRPC client. There is no
// single "the" LeaseMgr to connect to — Registry, Space, and EventMgr each
// grant their own leases and mount LeaseService on their own connection.
// Build one with DialLease (given a grantor address, typically read off a
// Lease's GrantorHost/GrantorPort) or NewLeaseClient (given an existing
// connection to a service you already dialed, e.g. via Client.Registry()).
type LeaseClient struct {
	client pb.LeaseServiceClient
	conn   *grpc.ClientConn // nil if built from an existing connection we don't own
}

// NewLeaseClient wraps an existing gRPC connection (e.g. one already held by
// a Client) as a LeaseClient. Does not take ownership — the caller still
// closes conn.
func NewLeaseClient(conn *grpc.ClientConn) *LeaseClient {
	return &LeaseClient{client: pb.NewLeaseServiceClient(conn)}
}

// LeaseDialOption configures DialLease.
type LeaseDialOption func(*leaseDialOptions)

type leaseDialOptions struct {
	token string
}

// WithLeaseToken attaches a bearer token to every call on this lease
// connection — needed against a grantor with COORDIN8_JWT_SECRET set.
func WithLeaseToken(token string) LeaseDialOption {
	return func(o *leaseDialOptions) { o.token = token }
}

// DialLease connects directly to a lease grantor's address — typically
// GrantorHost:GrantorPort read off a Lease you already hold. The returned
// LeaseClient owns the connection; call Close when done.
func DialLease(grantorAddr string, opts ...LeaseDialOption) (*LeaseClient, error) {
	cfg := &leaseDialOptions{}
	for _, o := range opts {
		o(cfg)
	}

	dialOpts := []grpc.DialOption{grpc.WithTransportCredentials(insecure.NewCredentials())}
	if cfg.token != "" {
		dialOpts = append(dialOpts, perRPCTokenOption(cfg.token))
	}

	conn, err := grpc.NewClient(grantorAddr, dialOpts...)
	if err != nil {
		return nil, fmt.Errorf("dial lease grantor %s: %w", grantorAddr, err)
	}
	return &LeaseClient{client: pb.NewLeaseServiceClient(conn), conn: conn}, nil
}

// Close releases the underlying connection if this LeaseClient owns one
// (built via DialLease). A no-op for one built via NewLeaseClient.
func (c *LeaseClient) Close() error {
	if c.conn != nil {
		return c.conn.Close()
	}
	return nil
}

// LeaseRecord is a live lease returned from the Djinn.
type LeaseRecord struct {
	LeaseID     string
	ResourceID  string
	GrantedAt   time.Time
	ExpiresAt   time.Time
	TTLSeconds  uint64
	GrantorHost string
	GrantorPort uint32
}

// GrantorAddr returns "host:port" for renewing/cancelling this lease — pass
// it to DialLease.
func (r LeaseRecord) GrantorAddr() string {
	return fmt.Sprintf("%s:%d", r.GrantorHost, r.GrantorPort)
}

func protoToRecord(l *pb.Lease) LeaseRecord {
	r := LeaseRecord{
		LeaseID:     l.LeaseId,
		ResourceID:  l.ResourceId,
		TTLSeconds:  l.TtlSeconds,
		GrantorHost: l.GrantorHost,
		GrantorPort: l.GrantorPort,
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
//
// Failures are reported on the returned channel — a transient transport
// error is retried on the next tick, while LeaseNotFound/LeaseExpired (the
// resource is genuinely gone) stop the loop after reporting. The channel is
// closed when KeepAlive returns; callers that don't care about failures can
// ignore it.
func (c *LeaseClient) KeepAlive(ctx context.Context, leaseID string, ttl time.Duration) <-chan error {
	failures := make(chan error, 1)
	go func() {
		defer close(failures)
		ticker := time.NewTicker(ttl / 2)
		defer ticker.Stop()
		for {
			select {
			case <-ticker.C:
				if _, err := c.Renew(ctx, leaseID, ttl); err != nil {
					select {
					case failures <- err:
					default:
					}
					code := status.Code(err)
					if code == codes.NotFound || code == codes.FailedPrecondition {
						// The resource is genuinely gone (LeaseNotFound /
						// LeaseExpired) — no point retrying.
						return
					}
					// Transient failure — keep trying on the next tick.
				}
			case <-ctx.Done():
				return
			}
		}
	}()
	return failures
}

// ExpiryEvent is delivered when a watched lease expires.
type ExpiryEvent struct {
	LeaseID    string
	ResourceID string
	ExpiredAt  time.Time
	Cancelled  bool // false = expired naturally, true = holder called Cancel
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
				Cancelled:  evt.Reason == pb.ReclaimReason_CANCELLED,
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
