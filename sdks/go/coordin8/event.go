package coordin8

import (
	"context"
	"io"
	"time"

	pb "github.com/coordin8/sdk-go/gen/coordin8"
)

// EventClient wraps the generated EventService gRPC client.
type EventClient struct {
	client pb.EventServiceClient
}

// Events returns a client for EventMgr operations.
func (c *Client) Events() *EventClient {
	return &EventClient{client: pb.NewEventServiceClient(c.eventConn)}
}

// EventRegistration is returned from Subscribe.
type EventRegistration struct {
	RegistrationID string
	Source         string
	LeaseID        string
	SeqNum         uint64
}

// Event is a single event delivered via Receive.
type Event struct {
	EventID   string
	Source    string
	EventType string
	SeqNum   uint64
	Attrs    map[string]string
	Payload  []byte
	Handback []byte
	EmittedAt time.Time
}

// SubscribeOpts configures a Subscribe call.
type SubscribeOpts struct {
	Source   string
	Template map[string]string
	Durable  bool
	TTL      time.Duration
	Handback []byte
}

// Subscribe creates a leased event subscription.
func (c *EventClient) Subscribe(ctx context.Context, opts SubscribeOpts) (*EventRegistration, error) {
	delivery := pb.DeliveryMode_BEST_EFFORT
	if opts.Durable {
		delivery = pb.DeliveryMode_DURABLE
	}
	resp, err := c.client.Subscribe(ctx, &pb.SubscribeRequest{
		Source:     opts.Source,
		Template:   opts.Template,
		Delivery:   delivery,
		TtlSeconds: uint64(opts.TTL.Seconds()),
		Handback:   opts.Handback,
	})
	if err != nil {
		return nil, err
	}
	leaseID := ""
	if resp.Lease != nil {
		leaseID = resp.Lease.LeaseId
	}
	return &EventRegistration{
		RegistrationID: resp.RegistrationId,
		Source:         resp.Source,
		LeaseID:        leaseID,
		SeqNum:         resp.SeqNum,
	}, nil
}

// Receive opens a server-streaming connection for events on a registration.
// Returns a channel that receives events until ctx is cancelled or the stream ends.
func (c *EventClient) Receive(ctx context.Context, registrationID string) (<-chan Event, error) {
	stream, err := c.client.Receive(ctx, &pb.ReceiveRequest{
		RegistrationId: registrationID,
	})
	if err != nil {
		return nil, err
	}

	ch := make(chan Event, 16)
	go func() {
		defer close(ch)
		for {
			evt, err := stream.Recv()
			if err == io.EOF {
				return
			}
			if err != nil {
				return
			}
			e := Event{
				EventID:   evt.EventId,
				Source:    evt.Source,
				EventType: evt.EventType,
				SeqNum:   evt.SeqNum,
				Attrs:    evt.Attrs,
				Payload:  evt.Payload,
				Handback: evt.Handback,
			}
			if evt.EmittedAt != nil {
				e.EmittedAt = evt.EmittedAt.AsTime()
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

// Emit publishes an event into the bus.
func (c *EventClient) Emit(ctx context.Context, source, eventType string, attrs map[string]string, payload []byte) error {
	_, err := c.client.Emit(ctx, &pb.EmitRequest{
		Source:    source,
		EventType: eventType,
		Attrs:    attrs,
		Payload:  payload,
	})
	return err
}

// RenewSubscription extends the lease on a subscription.
func (c *EventClient) RenewSubscription(ctx context.Context, registrationID string, ttl time.Duration) (LeaseRecord, error) {
	resp, err := c.client.RenewSubscription(ctx, &pb.RenewSubscriptionRequest{
		RegistrationId: registrationID,
		TtlSeconds:     uint64(ttl.Seconds()),
	})
	if err != nil {
		return LeaseRecord{}, err
	}
	return protoToRecord(resp), nil
}

// CancelSubscription removes a subscription.
func (c *EventClient) CancelSubscription(ctx context.Context, registrationID string) error {
	_, err := c.client.CancelSubscription(ctx, &pb.CancelSubscriptionRequest{
		RegistrationId: registrationID,
	})
	return err
}
