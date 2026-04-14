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
	proxyConn    *grpc.ClientConn
	spaceConn    *grpc.ClientConn
	eventConn    *grpc.ClientConn
}

// ConnectOption configures the Client.
type ConnectOption func(*connectOptions)

type connectOptions struct {
	leaseAddr    string
	registryAddr string
	proxyAddr    string
	spaceAddr    string
	eventAddr    string
}

// WithLeaseAddr overrides the default LeaseMgr address (default: host:9001).
func WithLeaseAddr(addr string) ConnectOption {
	return func(o *connectOptions) { o.leaseAddr = addr }
}

// WithRegistryAddr overrides the default Registry address (default: host:9002).
func WithRegistryAddr(addr string) ConnectOption {
	return func(o *connectOptions) { o.registryAddr = addr }
}

// WithProxyAddr overrides the default Proxy address (default: host:9003).
func WithProxyAddr(addr string) ConnectOption {
	return func(o *connectOptions) { o.proxyAddr = addr }
}

// WithSpaceAddr overrides the default Space address (default: host:9006).
func WithSpaceAddr(addr string) ConnectOption {
	return func(o *connectOptions) { o.spaceAddr = addr }
}

// WithEventAddr overrides the default EventMgr address (default: host:9005).
func WithEventAddr(addr string) ConnectOption {
	return func(o *connectOptions) { o.eventAddr = addr }
}

// Connect opens gRPC connections to LeaseMgr, Registry, and Proxy.
func Connect(host string, opts ...ConnectOption) (*Client, error) {
	cfg := &connectOptions{
		leaseAddr:    host + ":9001",
		registryAddr: host + ":9002",
		proxyAddr:    host + ":9003",
		spaceAddr:    host + ":9006",
		eventAddr:    host + ":9005",
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

	proxyConn, err := grpc.NewClient(cfg.proxyAddr, dialOpts...)
	if err != nil {
		leaseConn.Close()
		registryConn.Close()
		return nil, err
	}

	spaceConn, err := grpc.NewClient(cfg.spaceAddr, dialOpts...)
	if err != nil {
		leaseConn.Close()
		registryConn.Close()
		proxyConn.Close()
		return nil, err
	}

	eventConn, err := grpc.NewClient(cfg.eventAddr, dialOpts...)
	if err != nil {
		leaseConn.Close()
		registryConn.Close()
		proxyConn.Close()
		spaceConn.Close()
		return nil, err
	}

	return &Client{
		leaseConn:    leaseConn,
		registryConn: registryConn,
		proxyConn:    proxyConn,
		spaceConn:    spaceConn,
		eventConn:    eventConn,
	}, nil
}

// Close releases all gRPC connections.
func (c *Client) Close() error {
	c.leaseConn.Close()
	c.registryConn.Close()
	c.proxyConn.Close()
	c.spaceConn.Close()
	c.eventConn.Close()
	return nil
}
