package coordin8

import (
	"context"
	"sync"
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
	SeqNum    uint64
	Attrs     map[string]string
	Payload   []byte
	Handback  []byte
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

// AckInterval is how often Receive acknowledges the events the caller has
// taken off the channel.
const AckInterval = 250 * time.Millisecond

// Receive streams events for a registration until ctx is cancelled or the
// stream ends (then Err says why).
//
// DURABLE registrations are at-least-once: the server keeps each event in
// the mailbox until it is acknowledged. Receive acknowledges an event once
// the caller has taken it from the channel (C is unbuffered), in batches
// every AckInterval and when the stream ends. An event in flight when the
// connection drops is delivered again on the next Receive, so handle
// duplicates by SeqNum.
func (c *EventClient) Receive(ctx context.Context, registrationID string) (*Stream[Event], error) {
	stream, err := c.client.Receive(ctx, &pb.ReceiveRequest{
		RegistrationId: registrationID,
	})
	if err != nil {
		return nil, err
	}

	var mu sync.Mutex
	var taken, acked uint64
	flush := func(ctx context.Context) {
		mu.Lock()
		upTo := taken
		mu.Unlock()
		if upTo <= acked {
			return
		}
		if err := c.Ack(ctx, registrationID, upTo); err == nil {
			acked = upTo
		}
	}

	s := pump(ctx, 0, stream.Recv, func(evt *pb.Event) (Event, bool) {
		e := Event{
			EventID:   evt.EventId,
			Source:    evt.Source,
			EventType: evt.EventType,
			SeqNum:    evt.SeqNum,
			Attrs:     evt.Attrs,
			Payload:   evt.Payload,
			Handback:  evt.Handback,
		}
		if evt.EmittedAt != nil {
			e.EmittedAt = evt.EmittedAt.AsTime()
		}
		return e, true
	}, func(e Event) {
		mu.Lock()
		if e.SeqNum > taken {
			taken = e.SeqNum
		}
		mu.Unlock()
	})

	go func() {
		ticker := time.NewTicker(AckInterval)
		defer ticker.Stop()
		for {
			select {
			case <-ticker.C:
				flush(ctx)
			case <-s.Done():
				// Final ack on our own clock: ctx may be what ended it.
				fctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
				flush(fctx)
				cancel()
				return
			}
		}
	}()
	return s, nil
}

// Ack acknowledges every DURABLE event on the registration with
// SeqNum <= upToSeq, deleting it from the mailbox. Receive does this for
// you; call it directly only when consuming the stream some other way.
func (c *EventClient) Ack(ctx context.Context, registrationID string, upToSeq uint64) error {
	_, err := c.client.Ack(ctx, &pb.AckRequest{RegistrationId: registrationID, UpToSeq: upToSeq})
	return err
}

// Emit publishes an event into the bus.
func (c *EventClient) Emit(ctx context.Context, source, eventType string, attrs map[string]string, payload []byte) error {
	_, err := c.client.Emit(ctx, &pb.EmitRequest{
		Source:    source,
		EventType: eventType,
		Attrs:     attrs,
		Payload:   payload,
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
