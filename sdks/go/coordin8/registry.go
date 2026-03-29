package coordin8

import (
	"context"
	"io"
	"time"

	pb "github.com/coordin8/sdk-go/gen/coordin8"
)

// RegistryClient wraps the generated RegistryService gRPC client.
type RegistryClient struct {
	client pb.RegistryServiceClient
}

// Registry returns a client for Registry operations.
func (c *Client) Registry() *RegistryClient {
	return &RegistryClient{client: pb.NewRegistryServiceClient(c.registryConn)}
}

// Registration describes a service being registered with the Djinn.
type Registration struct {
	Interface string
	Attrs     map[string]string
	TTL       time.Duration
	Transport *TransportDescriptor
}

// TransportDescriptor describes how to reach a service.
type TransportDescriptor struct {
	Type   string
	Config map[string]string
}

// Capability is what the Registry returns — a resolved service handle.
type Capability struct {
	CapabilityID string
	Interface    string
	Attrs        map[string]string
	Transport    *TransportDescriptor
}

// Template is a partial attribute map for Registry lookups.
// Missing fields match anything. Present fields use exact match unless
// prefixed with an operator: "contains:foo", "starts_with:foo".
type Template map[string]string

func pbToCapability(c *pb.Capability) Capability {
	cap := Capability{
		CapabilityID: c.CapabilityId,
		Interface:    c.Interface,
		Attrs:        c.Attrs,
	}
	if c.Transport != nil {
		cap.Transport = &TransportDescriptor{
			Type:   c.Transport.Type,
			Config: c.Transport.Config,
		}
	}
	return cap
}

// Register registers a service with the Djinn. Returns the lease ID —
// renew it to stay registered, let it expire to disappear.
func (c *RegistryClient) Register(ctx context.Context, reg Registration) (string, error) {
	req := &pb.RegisterRequest{
		Interface:  reg.Interface,
		Attrs:      reg.Attrs,
		TtlSeconds: uint64(reg.TTL.Seconds()),
	}
	if reg.Transport != nil {
		req.Transport = &pb.TransportDescriptor{
			Type:   reg.Transport.Type,
			Config: reg.Transport.Config,
		}
	}
	resp, err := c.client.Register(ctx, req)
	if err != nil {
		return "", err
	}
	return resp.LeaseId, nil
}

// Lookup returns the first capability matching template.
func (c *RegistryClient) Lookup(ctx context.Context, tmpl Template) (Capability, error) {
	resp, err := c.client.Lookup(ctx, &pb.LookupRequest{Template: tmpl})
	if err != nil {
		return Capability{}, err
	}
	return pbToCapability(resp), nil
}

// LookupAll returns all capabilities matching template.
func (c *RegistryClient) LookupAll(ctx context.Context, tmpl Template) ([]Capability, error) {
	stream, err := c.client.LookupAll(ctx, &pb.LookupRequest{Template: tmpl})
	if err != nil {
		return nil, err
	}
	var caps []Capability
	for {
		c, err := stream.Recv()
		if err == io.EOF {
			break
		}
		if err != nil {
			return nil, err
		}
		caps = append(caps, pbToCapability(c))
	}
	return caps, nil
}

// RegistryEvent is delivered when a matching service registers or expires.
type RegistryEvent struct {
	Type       string // "registered", "expired", "renewed"
	Capability Capability
}

// Watch streams registry events for capabilities matching template.
func (c *RegistryClient) Watch(ctx context.Context, tmpl Template) (<-chan RegistryEvent, error) {
	stream, err := c.client.Watch(ctx, &pb.RegistryWatchRequest{Template: tmpl})
	if err != nil {
		return nil, err
	}

	ch := make(chan RegistryEvent, 16)
	go func() {
		defer close(ch)
		for {
			evt, err := stream.Recv()
			if err != nil {
				return
			}
			typeStr := "registered"
			switch evt.Type {
			case pb.RegistryEvent_EXPIRED:
				typeStr = "expired"
			case pb.RegistryEvent_RENEWED:
				typeStr = "renewed"
			}
			e := RegistryEvent{Type: typeStr}
			if evt.Capability != nil {
				e.Capability = pbToCapability(evt.Capability)
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
