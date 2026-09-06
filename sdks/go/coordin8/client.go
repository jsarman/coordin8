// Package coordin8 provides the Go client SDK for the Coordin8 Djinn.
//
// Usage:
//
//	djinn, err := coordin8.Connect("localhost:9002")
//	defer djinn.Close()
//
//	leases := djinn.Leases()
//	registry := djinn.Registry()
package coordin8

import (
	"context"
	"fmt"
	"time"

	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"

	pb "github.com/coordin8/sdk-go/gen/coordin8"
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
	leaseAddr string
	proxyAddr string
	spaceAddr string
	eventAddr string
}

// WithLeaseAddr pins the LeaseMgr address instead of looking it up through
// Registry. Useful for tests, or reaching a service on a network segment
// Registry's lookup result isn't itself reachable from.
func WithLeaseAddr(addr string) ConnectOption {
	return func(o *connectOptions) { o.leaseAddr = addr }
}

// WithProxyAddr pins the Proxy address instead of looking it up through Registry.
func WithProxyAddr(addr string) ConnectOption {
	return func(o *connectOptions) { o.proxyAddr = addr }
}

// WithSpaceAddr pins the Space address instead of looking it up through Registry.
func WithSpaceAddr(addr string) ConnectOption {
	return func(o *connectOptions) { o.spaceAddr = addr }
}

// WithEventAddr pins the EventMgr address instead of looking it up through Registry.
func WithEventAddr(addr string) ConnectOption {
	return func(o *connectOptions) { o.eventAddr = addr }
}

// Connect dials Registry directly at registryAddr — the one address a
// caller needs to know in advance — then looks up LeaseMgr, Proxy, Space,
// and EventMgr through it, the same way any application service is
// discovered via ServiceDiscovery. Works identically against a bundled
// monolith or a fully split, multi-host deployment: Registry just returns
// whatever address each service actually registered.
//
// Use WithLeaseAddr / WithProxyAddr / WithSpaceAddr / WithEventAddr to pin
// a specific service's address instead of looking it up.
func Connect(registryAddr string, opts ...ConnectOption) (*Client, error) {
	cfg := &connectOptions{}
	for _, o := range opts {
		o(cfg)
	}

	dialOpts := []grpc.DialOption{
		grpc.WithTransportCredentials(insecure.NewCredentials()),
	}

	registryConn, err := grpc.NewClient(registryAddr, dialOpts...)
	if err != nil {
		return nil, fmt.Errorf("connect to registry: %w", err)
	}
	registryClient := pb.NewRegistryServiceClient(registryConn)

	resolve := func(pinned, interfaceName string) (string, error) {
		if pinned != "" {
			return pinned, nil
		}
		return lookupAddr(registryClient, interfaceName)
	}

	leaseAddr, err := resolve(cfg.leaseAddr, "LeaseMgr")
	if err != nil {
		registryConn.Close()
		return nil, err
	}
	proxyAddr, err := resolve(cfg.proxyAddr, "Proxy")
	if err != nil {
		registryConn.Close()
		return nil, err
	}
	spaceAddr, err := resolve(cfg.spaceAddr, "Space")
	if err != nil {
		registryConn.Close()
		return nil, err
	}
	eventAddr, err := resolve(cfg.eventAddr, "EventMgr")
	if err != nil {
		registryConn.Close()
		return nil, err
	}

	leaseConn, err := grpc.NewClient(leaseAddr, dialOpts...)
	if err != nil {
		registryConn.Close()
		return nil, err
	}

	proxyConn, err := grpc.NewClient(proxyAddr, dialOpts...)
	if err != nil {
		leaseConn.Close()
		registryConn.Close()
		return nil, err
	}

	spaceConn, err := grpc.NewClient(spaceAddr, dialOpts...)
	if err != nil {
		leaseConn.Close()
		registryConn.Close()
		proxyConn.Close()
		return nil, err
	}

	eventConn, err := grpc.NewClient(eventAddr, dialOpts...)
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

// lookupAddr looks up interfaceName in Registry and returns its "host:port"
// transport address.
func lookupAddr(registryClient pb.RegistryServiceClient, interfaceName string) (string, error) {
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	resp, err := registryClient.Lookup(ctx, &pb.LookupRequest{
		Template: map[string]string{"interface": interfaceName},
	})
	if err != nil {
		return "", fmt.Errorf("look up %s: %w", interfaceName, err)
	}
	if resp.Transport == nil {
		return "", fmt.Errorf("look up %s: no transport in registry entry", interfaceName)
	}
	host, port := resp.Transport.Config["host"], resp.Transport.Config["port"]
	if host == "" || port == "" {
		return "", fmt.Errorf("look up %s: missing host/port in transport config", interfaceName)
	}
	return host + ":" + port, nil
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
