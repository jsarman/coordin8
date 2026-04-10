package coordin8

import (
	"context"
	"io"
	"time"

	pb "github.com/coordin8/sdk-go/gen/coordin8"
)

// SpaceClient wraps the generated SpaceService gRPC client.
type SpaceClient struct {
	client pb.SpaceServiceClient
}

// Space returns a client for Space (tuple store) operations.
func (c *Client) Space() *SpaceClient {
	return &SpaceClient{client: pb.NewSpaceServiceClient(c.spaceConn)}
}

// TupleRecord is a tuple stored in the Space.
type TupleRecord struct {
	TupleID      string
	Attrs        map[string]string
	Payload      []byte
	LeaseID      string
	WrittenBy    string
	WrittenAt    time.Time
	InputTupleID string
}

func protoToTuple(t *pb.Tuple) *TupleRecord {
	if t == nil {
		return nil
	}
	r := &TupleRecord{
		TupleID: t.TupleId,
		Attrs:   t.Attrs,
		Payload: t.Payload,
	}
	if t.Lease != nil {
		r.LeaseID = t.Lease.LeaseId
	}
	if t.Provenance != nil {
		r.WrittenBy = t.Provenance.WrittenBy
		r.InputTupleID = t.Provenance.InputTupleId
		if t.Provenance.WrittenAt != nil {
			r.WrittenAt = t.Provenance.WrittenAt.AsTime()
		}
	}
	return r
}

// WriteOpts configures a Space Write call.
type WriteOpts struct {
	// Attrs are the indexed fields for template matching.
	Attrs map[string]string
	// Payload is opaque data stored alongside the attrs.
	Payload []byte
	// TTL is the lease duration for the tuple.
	TTL time.Duration
	// WrittenBy identifies the writer (provenance).
	WrittenBy string
	// InputTupleID links this tuple to its predecessor (lineage).
	InputTupleID string
	// TxnID makes the write transactional (optional).
	TxnID string
}

// Write stores a tuple in the Space. Returns the written tuple.
func (c *SpaceClient) Write(ctx context.Context, opts WriteOpts) (*TupleRecord, error) {
	resp, err := c.client.Write(ctx, &pb.WriteRequest{
		Attrs:        opts.Attrs,
		Payload:      opts.Payload,
		TtlSeconds:   uint64(opts.TTL.Seconds()),
		WrittenBy:    opts.WrittenBy,
		InputTupleId: opts.InputTupleID,
		TxnId:        opts.TxnID,
	})
	if err != nil {
		return nil, err
	}
	return protoToTuple(resp.GetTuple()), nil
}

// ReadOpts configures a Space Read call.
type ReadOpts struct {
	// Template is the attribute pattern to match.
	Template map[string]string
	// Wait blocks until a match arrives (true) or returns immediately (false).
	Wait bool
	// Timeout is the max wait duration. Zero with Wait=true means wait forever.
	Timeout time.Duration
	// TxnID scopes the read to a transaction (optional).
	TxnID string
}

// Read finds a tuple matching the template without removing it.
// Returns nil when no match is found (non-blocking or timeout).
func (c *SpaceClient) Read(ctx context.Context, opts ReadOpts) (*TupleRecord, error) {
	resp, err := c.client.Read(ctx, &pb.ReadRequest{
		Template:  opts.Template,
		Wait:      opts.Wait,
		TimeoutMs: uint64(opts.Timeout.Milliseconds()),
		TxnId:     opts.TxnID,
	})
	if err != nil {
		return nil, err
	}
	return protoToTuple(resp.GetTuple()), nil
}

// TakeOpts configures a Space Take call.
type TakeOpts struct {
	// Template is the attribute pattern to match.
	Template map[string]string
	// Wait blocks until a match arrives (true) or returns immediately (false).
	Wait bool
	// Timeout is the max wait duration. Zero with Wait=true means wait forever.
	Timeout time.Duration
	// TxnID scopes the take to a transaction (optional).
	TxnID string
}

// Take atomically claims and removes a matching tuple.
// Returns nil when no match is found (non-blocking or timeout).
func (c *SpaceClient) Take(ctx context.Context, opts TakeOpts) (*TupleRecord, error) {
	resp, err := c.client.Take(ctx, &pb.TakeRequest{
		Template:  opts.Template,
		Wait:      opts.Wait,
		TimeoutMs: uint64(opts.Timeout.Milliseconds()),
		TxnId:     opts.TxnID,
	})
	if err != nil {
		return nil, err
	}
	return protoToTuple(resp.GetTuple()), nil
}

// Contents returns all tuples matching the template without removing them.
func (c *SpaceClient) Contents(ctx context.Context, template map[string]string, txnID string) ([]*TupleRecord, error) {
	stream, err := c.client.Contents(ctx, &pb.ContentsRequest{
		Template: template,
		TxnId:    txnID,
	})
	if err != nil {
		return nil, err
	}

	var results []*TupleRecord
	for {
		t, err := stream.Recv()
		if err == io.EOF {
			break
		}
		if err != nil {
			return results, err
		}
		results = append(results, protoToTuple(t))
	}
	return results, nil
}

// SpaceEventType mirrors the proto enum.
type SpaceEventType int

const (
	Appearance SpaceEventType = 0
	Expiration SpaceEventType = 1
)

// SpaceEvent is delivered when a watched tuple appears or expires.
type SpaceEvent struct {
	Type       SpaceEventType
	Tuple      *TupleRecord
	OccurredAt time.Time
	Handback   []byte
}

// NotifyOpts configures a Space Notify subscription.
type NotifyOpts struct {
	// Template is the attribute pattern to watch.
	Template map[string]string
	// On selects the event type (Appearance or Expiration).
	On SpaceEventType
	// TTL is the lease duration for the watch itself.
	TTL time.Duration
	// Handback is opaque data echoed with each event.
	Handback []byte
}

// Notify subscribes to tuple events matching the template.
// Returns a channel that receives events until ctx is cancelled or the
// watch lease expires.
func (c *SpaceClient) Notify(ctx context.Context, opts NotifyOpts) (<-chan SpaceEvent, error) {
	stream, err := c.client.Notify(ctx, &pb.NotifyRequest{
		Template:   opts.Template,
		On:         pb.SpaceEventType(opts.On),
		TtlSeconds: uint64(opts.TTL.Seconds()),
		Handback:   opts.Handback,
	})
	if err != nil {
		return nil, err
	}

	ch := make(chan SpaceEvent, 16)
	go func() {
		defer close(ch)
		for {
			evt, err := stream.Recv()
			if err != nil {
				return
			}
			se := SpaceEvent{
				Type:     SpaceEventType(evt.Type),
				Tuple:    protoToTuple(evt.Tuple),
				Handback: evt.Handback,
			}
			if evt.OccurredAt != nil {
				se.OccurredAt = evt.OccurredAt.AsTime()
			}
			select {
			case ch <- se:
			case <-ctx.Done():
				return
			}
		}
	}()
	return ch, nil
}

// RenewTuple extends the lease on a tuple already in the Space.
func (c *SpaceClient) RenewTuple(ctx context.Context, tupleID string, ttl time.Duration) (LeaseRecord, error) {
	resp, err := c.client.Renew(ctx, &pb.RenewTupleRequest{
		TupleId:    tupleID,
		TtlSeconds: uint64(ttl.Seconds()),
	})
	if err != nil {
		return LeaseRecord{}, err
	}
	return protoToRecord(resp), nil
}

// CancelTuple removes a tuple from the Space and cancels its lease.
func (c *SpaceClient) CancelTuple(ctx context.Context, tupleID string) error {
	_, err := c.client.Cancel(ctx, &pb.CancelTupleRequest{
		TupleId: tupleID,
	})
	return err
}
