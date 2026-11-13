package coordin8

import (
	"context"
	"sync"
	"testing"
	"time"

	pb "github.com/coordin8/sdk-go/gen/coordin8"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
	"google.golang.org/protobuf/types/known/emptypb"
)

// fakeEvents streams `events`, then ends with `end` (nil: wait for the
// client to leave). Acks are recorded.
type fakeEvents struct {
	pb.UnimplementedEventServiceServer
	events []*pb.Event
	end    error
	mu     sync.Mutex
	acks   []uint64
}

func (f *fakeEvents) Receive(_ *pb.ReceiveRequest, s pb.EventService_ReceiveServer) error {
	for _, e := range f.events {
		if err := s.Send(e); err != nil {
			return err
		}
	}
	if f.end != nil {
		return f.end
	}
	<-s.Context().Done()
	return nil
}

func (f *fakeEvents) Ack(_ context.Context, r *pb.AckRequest) (*emptypb.Empty, error) {
	f.mu.Lock()
	f.acks = append(f.acks, r.UpToSeq)
	f.mu.Unlock()
	return &emptypb.Empty{}, nil
}

func (f *fakeEvents) lastAck() uint64 {
	f.mu.Lock()
	defer f.mu.Unlock()
	if len(f.acks) == 0 {
		return 0
	}
	return f.acks[len(f.acks)-1]
}

func newEventClient(t *testing.T, f *fakeEvents) *EventClient {
	t.Helper()
	addr, _ := startFake(t, func(s *grpc.Server) { pb.RegisterEventServiceServer(s, f) })
	c, err := Connect(addr, WithProxyAddr(addr), WithSpaceAddr(addr), WithEventAddr(addr))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { c.Close() })
	return c.Events()
}

// Only events the caller has taken are acknowledged; one still in flight
// when the stream ends is left for redelivery.
func TestReceiveAcksWhatTheCallerTook(t *testing.T) {
	f := &fakeEvents{events: []*pb.Event{{SeqNum: 1}, {SeqNum: 2}, {SeqNum: 3}}}
	ec := newEventClient(t, f)
	ctx, cancel := context.WithCancel(context.Background())
	s, err := ec.Receive(ctx, "reg")
	if err != nil {
		t.Fatal(err)
	}
	for want := uint64(1); want <= 2; want++ {
		if e := <-s.C; e.SeqNum != want {
			t.Fatalf("seq = %d, want %d", e.SeqNum, want)
		}
	}
	eventually(t, "periodic ack of 2", func() bool { return f.lastAck() == 2 })
	cancel()
	<-s.Done()
	time.Sleep(2 * AckInterval)
	if got := f.lastAck(); got != 2 {
		t.Fatalf("acked through %d; event 3 was never taken and must stay unacked", got)
	}
	if s.Err() != nil {
		t.Fatalf("cancelled stream should end clean, got %v", s.Err())
	}
}

// The server's reason for ending a stream reaches the caller.
func TestReceiveSurfacesServerError(t *testing.T) {
	f := &fakeEvents{events: []*pb.Event{{SeqNum: 1}}, end: status.Error(codes.DataLoss, "lagged")}
	ec := newEventClient(t, f)
	s, err := ec.Receive(context.Background(), "reg")
	if err != nil {
		t.Fatal(err)
	}
	for range s.C {
	}
	if status.Code(s.Err()) != codes.DataLoss {
		t.Fatalf("Err() = %v, want DataLoss", s.Err())
	}
}

// fakeNotify sends the REGISTERED lease, one appearance, then waits until
// `end` is closed and ends NOT_FOUND (the watch lease was reclaimed).
type fakeNotify struct {
	pb.UnimplementedSpaceServiceServer
	end    chan struct{}
	leases *fakeRenewals
}

// fakeRenewals is Space's LeaseService: it records renewals.
type fakeRenewals struct {
	pb.UnimplementedLeaseServiceServer
	mu     sync.Mutex
	renews []string
}

func (f *fakeNotify) Notify(_ *pb.NotifyRequest, s pb.SpaceService_NotifyServer) error {
	if err := s.Send(&pb.SpaceEvent{Type: pb.SpaceEventType_REGISTERED,
		Lease: &pb.Lease{LeaseId: "watch-lease", TtlSeconds: 1}}); err != nil {
		return err
	}
	if err := s.Send(&pb.SpaceEvent{Type: pb.SpaceEventType_APPEARANCE,
		Tuple: &pb.Tuple{TupleId: "t1"}}); err != nil {
		return err
	}
	select {
	case <-f.end:
		return status.Error(codes.NotFound, "watch lease reclaimed")
	case <-s.Context().Done():
		return nil
	}
}

func (f *fakeRenewals) Renew(_ context.Context, r *pb.RenewRequest) (*pb.Lease, error) {
	f.mu.Lock()
	f.renews = append(f.renews, r.LeaseId)
	f.mu.Unlock()
	return &pb.Lease{LeaseId: r.LeaseId, TtlSeconds: r.TtlSeconds}, nil
}

func (f *fakeRenewals) renewCount() int {
	f.mu.Lock()
	defer f.mu.Unlock()
	return len(f.renews)
}

func TestNotifyRenewsItsWatchLeaseAndReportsReclaim(t *testing.T) {
	f := &fakeNotify{end: make(chan struct{}), leases: &fakeRenewals{}}
	addr, _ := startFake(t, func(s *grpc.Server) {
		pb.RegisterSpaceServiceServer(s, f)
		pb.RegisterLeaseServiceServer(s, f.leases)
	})
	c, err := Connect(addr, WithProxyAddr(addr), WithSpaceAddr(addr), WithEventAddr(addr))
	if err != nil {
		t.Fatal(err)
	}
	defer c.Close()

	w, err := c.Space().Notify(context.Background(), NotifyOpts{TTL: time.Second})
	if err != nil {
		t.Fatal(err)
	}
	if w.Lease.LeaseID != "watch-lease" {
		t.Fatalf("lease = %+v", w.Lease)
	}
	if e := <-w.C; e.Tuple == nil || e.Tuple.TupleID != "t1" {
		t.Fatalf("first event should be the appearance, got %+v", e)
	}
	eventually(t, "watch lease renewal", func() bool { return f.leases.renewCount() > 0 })

	close(f.end)
	for range w.C {
	}
	if status.Code(w.Err()) != codes.NotFound {
		t.Fatalf("Err() = %v, want NotFound", w.Err())
	}
	n := f.leases.renewCount()
	time.Sleep(700 * time.Millisecond)
	if f.leases.renewCount() != n {
		t.Fatal("renewals should stop once the watch ended")
	}
}
