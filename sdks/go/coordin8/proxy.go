package coordin8

import (
	"context"
	"fmt"
	"sync"
	"time"

	pb "github.com/coordin8/sdk-go/gen/coordin8"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/credentials/insecure"
	"google.golang.org/grpc/status"
)

// DefaultProxyTTL is the lease TTL Open requests for a proxy. The SDK renews
// it in the background until Release, so this only bounds how long an
// abandoned proxy (crashed client) lingers on the Djinn.
const DefaultProxyTTL = 30 * time.Second

// ProxyHandle holds a reference to an open Djinn proxy. The proxy is leased:
// the SDK renews the lease in the background (against the Proxy's own
// LeaseService) until Release is called.
type ProxyHandle struct {
	ProxyID   string
	LocalPort int32
	Lease     LeaseRecord
	client    *ProxyClient

	stopKeepAlive context.CancelFunc
	stopped       chan struct{} // closed by Release
	lost          chan struct{} // closed when keepAlive reports the lease gone
	stopOnce      sync.Once
}

// Lost returns a channel that is closed when the proxy's lease is gone
// (NOT_FOUND / FAILED_PRECONDITION on renew) — the Djinn has reclaimed the
// proxy and it must be reopened.
func (h *ProxyHandle) Lost() <-chan struct{} { return h.lost }

// Release stops lease renewal and releases the proxy on the Djinn.
func (h *ProxyHandle) Release(ctx context.Context) error {
	h.stopOnce.Do(func() {
		h.stopKeepAlive()
		close(h.stopped)
	})
	return h.client.Release(ctx, h.ProxyID)
}

// ProxyClient wraps the generated ProxyService gRPC client.
type ProxyClient struct {
	client pb.ProxyServiceClient
	leases *LeaseClient // LeaseService mounted on Proxy's own connection
}

// Proxy returns a client for Proxy operations.
func (c *Client) Proxy() *ProxyClient {
	return &ProxyClient{client: pb.NewProxyServiceClient(c.proxyConn), leases: NewLeaseClient(c.proxyConn)}
}

// Open asks the Djinn to open a local TCP forwarding port for the given
// template, with a DefaultProxyTTL lease kept alive in the background.
// Call ProxyHandle.Release when done.
func (c *ProxyClient) Open(ctx context.Context, tmpl Template) (*ProxyHandle, error) {
	return c.OpenWithTTL(ctx, tmpl, DefaultProxyTTL)
}

// OpenWithTTL is Open with an explicit lease TTL (0 = server's preferred TTL).
func (c *ProxyClient) OpenWithTTL(ctx context.Context, tmpl Template, ttl time.Duration) (*ProxyHandle, error) {
	resp, err := c.client.Open(ctx, &pb.OpenRequest{Template: tmpl, TtlSeconds: uint64(ttl.Seconds())})
	if err != nil {
		return nil, err
	}
	h := &ProxyHandle{
		ProxyID:   resp.ProxyId,
		LocalPort: resp.LocalPort,
		client:    c,
		stopped:   make(chan struct{}),
		lost:      make(chan struct{}),
	}
	if resp.Lease != nil {
		h.Lease = protoToRecord(resp.Lease)
		// Renew with the TTL as granted (the server may have negotiated it).
		granted := time.Duration(resp.Lease.TtlSeconds) * time.Second
		if granted <= 0 {
			granted = ttl
		}
		kctx, cancel := context.WithCancel(context.Background())
		h.stopKeepAlive = cancel
		failures := c.leases.KeepAlive(kctx, h.Lease.LeaseID, granted)
		go func() {
			for err := range failures {
				if code := status.Code(err); code == codes.NotFound || code == codes.FailedPrecondition {
					close(h.lost)
					return
				}
			}
		}()
	} else {
		h.stopKeepAlive = func() {}
	}
	return h, nil
}

// Release releases a proxy on the Djinn.
func (c *ProxyClient) Release(ctx context.Context, proxyID string) error {
	_, err := c.client.Release(ctx, &pb.ReleaseRequest{ProxyId: proxyID})
	return err
}

// ProxyConn opens a Djinn proxy and returns a ready *grpc.ClientConn pointed at it.
// The returned cleanup function closes both the gRPC connection and the proxy.
// Typical use:
//
//	conn, cleanup, err := djinn.Proxy().ProxyConn(ctx, coordin8.Template{"interface": "Greeter"})
//	defer cleanup()
//	stub := hello.NewGreeterServiceClient(conn)
func (c *ProxyClient) ProxyConn(ctx context.Context, tmpl Template) (*grpc.ClientConn, func(), error) {
	handle, err := c.Open(ctx, tmpl)
	if err != nil {
		return nil, nil, err
	}

	addr := fmt.Sprintf("localhost:%d", handle.LocalPort)
	conn, err := grpc.NewClient(addr, grpc.WithTransportCredentials(insecure.NewCredentials()))
	if err != nil {
		_ = handle.Release(ctx)
		return nil, nil, err
	}

	cleanup := func() {
		conn.Close()
		_ = handle.Release(context.Background())
	}
	return conn, cleanup, nil
}
