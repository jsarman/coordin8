package coordin8

import (
	"context"
	"fmt"

	pb "github.com/coordin8/sdk-go/gen/coordin8"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"
)

// ProxyHandle holds a reference to an open Djinn proxy.
type ProxyHandle struct {
	ProxyID   string
	LocalPort int32
	client    *ProxyClient
}

// Release releases the proxy on the Djinn.
func (h *ProxyHandle) Release(ctx context.Context) error {
	return h.client.Release(ctx, h.ProxyID)
}

// ProxyClient wraps the generated ProxyService gRPC client.
type ProxyClient struct {
	client pb.ProxyServiceClient
}

// Proxy returns a client for Proxy operations.
func (c *Client) Proxy() *ProxyClient {
	return &ProxyClient{client: pb.NewProxyServiceClient(c.proxyConn)}
}

// Open asks the Djinn to open a local TCP forwarding port for the given template.
// Call ProxyHandle.Close when done.
func (c *ProxyClient) Open(ctx context.Context, tmpl Template) (*ProxyHandle, error) {
	resp, err := c.client.Open(ctx, &pb.OpenRequest{Template: tmpl})
	if err != nil {
		return nil, err
	}
	return &ProxyHandle{
		ProxyID:   resp.ProxyId,
		LocalPort: resp.LocalPort,
		client:    c,
	}, nil
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
		_ = c.Release(ctx, handle.ProxyID)
		return nil, nil, err
	}

	cleanup := func() {
		conn.Close()
		_ = c.Release(context.Background(), handle.ProxyID)
	}
	return conn, cleanup, nil
}
