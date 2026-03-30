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
	// Set to update an existing registration in-place (re-registration).
	// Leave empty for a new registration.
	CapabilityID string
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

// RegisterResult holds the server-assigned capability ID and lease ID.
type RegisterResult struct {
	CapabilityID string
	LeaseID      string
}

// Register registers a service with the Djinn. Returns the server-assigned
// capability ID and lease ID. Pass the capability ID back on subsequent
// Register calls to update an existing registration in-place.
func (c *RegistryClient) Register(ctx context.Context, reg Registration) (RegisterResult, error) {
	req := &pb.RegisterRequest{
		Interface:    reg.Interface,
		Attrs:        reg.Attrs,
		TtlSeconds:   uint64(reg.TTL.Seconds()),
		CapabilityId: reg.CapabilityID,
	}
	if reg.Transport != nil {
		req.Transport = &pb.TransportDescriptor{
			Type:   reg.Transport.Type,
			Config: reg.Transport.Config,
		}
	}
	resp, err := c.client.Register(ctx, req)
	if err != nil {
		return RegisterResult{}, err
	}
	leaseID := ""
	if resp.Lease != nil {
		leaseID = resp.Lease.LeaseId
	}
	return RegisterResult{
		CapabilityID: resp.CapabilityId,
		LeaseID:      leaseID,
	}, nil
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

// RegistryEvent is delivered when a matching service registers, expires, or is modified.
type RegistryEvent struct {
	Type       string // "registered", "expired", "modified"
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
			case pb.RegistryEvent_MODIFIED:
				typeStr = "modified"
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
