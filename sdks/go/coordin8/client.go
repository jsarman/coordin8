// Package coordin8 provides the Go client SDK for the Coordin8 Djinn.
//
// Usage:
//
//	djinn, err := coordin8.Connect("localhost")
//	defer djinn.Close()
//
//	leases := djinn.Leases()
//	registry := djinn.Registry()
package coordin8

import (
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"
)

// Client is the entry point for all Djinn interactions.
type Client struct {
	leaseConn    *grpc.ClientConn
	registryConn *grpc.ClientConn
}

// ConnectOption configures the Client.
type ConnectOption func(*connectOptions)

type connectOptions struct {
	leaseAddr    string
	registryAddr string
}

// WithLeaseAddr overrides the default LeaseMgr address (default: host:9001).
func WithLeaseAddr(addr string) ConnectOption {
	return func(o *connectOptions) { o.leaseAddr = addr }
}

// WithRegistryAddr overrides the default Registry address (default: host:9002).
func WithRegistryAddr(addr string) ConnectOption {
	return func(o *connectOptions) { o.registryAddr = addr }
}

// Connect opens gRPC connections to LeaseMgr and Registry.
func Connect(host string, opts ...ConnectOption) (*Client, error) {
	cfg := &connectOptions{
		leaseAddr:    host + ":9001",
		registryAddr: host + ":9002",
	}
	for _, o := range opts {
		o(cfg)
	}

	dialOpts := []grpc.DialOption{
		grpc.WithTransportCredentials(insecure.NewCredentials()),
	}

	leaseConn, err := grpc.NewClient(cfg.leaseAddr, dialOpts...)
	if err != nil {
		return nil, err
	}

	registryConn, err := grpc.NewClient(cfg.registryAddr, dialOpts...)
	if err != nil {
		leaseConn.Close()
		return nil, err
	}

	return &Client{
		leaseConn:    leaseConn,
		registryConn: registryConn,
	}, nil
}

// Close releases all gRPC connections.
func (c *Client) Close() error {
	c.leaseConn.Close()
	c.registryConn.Close()
	return nil
}
