package coordin8

import (
	"context"
	"sync/atomic"
	"testing"
	"time"

	pb "github.com/coordin8/sdk-go/gen/coordin8"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/credentials/insecure"
	"google.golang.org/grpc/status"
	"google.golang.org/protobuf/types/known/emptypb"
	"google.golang.org/protobuf/types/known/timestamppb"
)

// fakeLease implements LeaseService with a scriptable Renew.
type fakeLease struct {
	pb.UnimplementedLeaseServiceServer
	renew    func(n int32, req *pb.RenewRequest) (*pb.Lease, error)
	renews   atomic.Int32
	cancelID atomic.Value
	events   []*pb.ExpiryEvent
}

func (f *fakeLease) Grant(_ context.Context, r *pb.GrantRequest) (*pb.Lease, error) {
	return &pb.Lease{LeaseId: "L1", ResourceId: r.ResourceId, TtlSeconds: r.TtlSeconds,
		GrantorHost: "h", GrantorPort: 9,
		GrantedAt: timestamppb.New(time.Unix(100, 0)), ExpiresAt: timestamppb.New(time.Unix(200, 0))}, nil
}

func (f *fakeLease) Renew(_ context.Context, r *pb.RenewRequest) (*pb.Lease, error) {
	n := f.renews.Add(1)
	return f.renew(n, r)
}

func (f *fakeLease) Cancel(_ context.Context, r *pb.CancelRequest) (*emptypb.Empty, error) {
	f.cancelID.Store(r.LeaseId)
	return &emptypb.Empty{}, nil
}

func (f *fakeLease) WatchExpiry(_ *pb.WatchExpiryRequest, s pb.LeaseService_WatchExpiryServer) error {
	for _, e := range f.events {
		if err := s.Send(e); err != nil {
			return err
		}
	}
	return nil
}

func dialFakeLease(t *testing.T, f *fakeLease, opts ...LeaseDialOption) (*LeaseClient, *authRecorder) {
	t.Helper()
	addr, rec := startFake(t, func(s *grpc.Server) { pb.RegisterLeaseServiceServer(s, f) })
	lc, err := DialLease(addr, opts...)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { lc.Close() })
	return lc, rec
}

func okRenew(n int32, r *pb.RenewRequest) (*pb.Lease, error) {
	return &pb.Lease{LeaseId: r.LeaseId, TtlSeconds: r.TtlSeconds}, nil
}

func TestGrantMapsRequestAndResponse(t *testing.T) {
	lc, _ := dialFakeLease(t, &fakeLease{renew: okRenew})
	rec, err := lc.Grant(context.Background(), "res", 30*time.Second)
	if err != nil {
		t.Fatal(err)
	}
	if rec.LeaseID != "L1" || rec.ResourceID != "res" || rec.TTLSeconds != 30 {
		t.Fatalf("bad record: %+v", rec)
	}
	if rec.GrantorAddr() != "h:9" {
		t.Fatalf("GrantorAddr = %q", rec.GrantorAddr())
	}
	if !rec.GrantedAt.Equal(time.Unix(100, 0)) || !rec.ExpiresAt.Equal(time.Unix(200, 0)) {
		t.Fatalf("bad times: %+v", rec)
	}
}

func TestCancelSendsLeaseID(t *testing.T) {
	f := &fakeLease{renew: okRenew}
	lc, _ := dialFakeLease(t, f)
	if err := lc.Cancel(context.Background(), "abc"); err != nil {
		t.Fatal(err)
	}
	if f.cancelID.Load() != "abc" {
		t.Fatalf("cancel id = %v", f.cancelID.Load())
	}
}

func TestWatchMapsExpiryEvents(t *testing.T) {
	f := &fakeLease{renew: okRenew, events: []*pb.ExpiryEvent{
		{LeaseId: "a", ResourceId: "r", Reason: pb.ReclaimReason_EXPIRED, ExpiredAt: timestamppb.New(time.Unix(5, 0))},
		{LeaseId: "b", ResourceId: "r", Reason: pb.ReclaimReason_CANCELLED},
	}}
	lc, _ := dialFakeLease(t, f)
	ch, err := lc.Watch(context.Background(), "r")
	if err != nil {
		t.Fatal(err)
	}
	e1, e2 := <-ch, <-ch
	if e1.LeaseID != "a" || e1.Cancelled || !e1.ExpiredAt.Equal(time.Unix(5, 0)) {
		t.Fatalf("e1 = %+v", e1)
	}
	if e2.LeaseID != "b" || !e2.Cancelled {
		t.Fatalf("e2 = %+v", e2)
	}
	if _, ok := <-ch; ok {
		t.Fatal("channel should close when stream ends")
	}
}

func TestDialLeaseWithTokenAttachesBearer(t *testing.T) {
	lc, rec := dialFakeLease(t, &fakeLease{renew: okRenew}, WithLeaseToken("tok123"))
	if _, err := lc.Grant(context.Background(), "r", time.Second); err != nil {
		t.Fatal(err)
	}
	if got := rec.all(); len(got) != 1 || got[0] != "Bearer tok123" {
		t.Fatalf("auth metadata = %v", got)
	}
}

func TestDialLeaseWithoutTokenSendsNone(t *testing.T) {
	lc, rec := dialFakeLease(t, &fakeLease{renew: okRenew})
	_, _ = lc.Grant(context.Background(), "r", time.Second)
	if got := rec.all(); len(got) != 1 || got[0] != "" {
		t.Fatalf("auth metadata = %v", got)
	}
}

func TestKeepAliveRenewsUntilContextCancelled(t *testing.T) {
	f := &fakeLease{renew: okRenew}
	lc, _ := dialFakeLease(t, f)
	ctx, cancel := context.WithCancel(context.Background())
	failures := lc.KeepAlive(ctx, "L1", 40*time.Millisecond) // tick every 20ms
	eventually(t, "3 renewals", func() bool { return f.renews.Load() >= 3 })
	cancel()
	select {
	case err, ok := <-failures:
		// A renewal in flight at cancel time may report a Canceled error
		// before the channel closes; anything else is a bug.
		if ok && status.Code(err) != codes.Canceled {
			t.Fatalf("unexpected failure: %v", err)
		}
	case <-time.After(2 * time.Second):
		t.Fatal("failures channel not closed after cancel")
	}
}

func TestKeepAliveStopsOnTerminalErrors(t *testing.T) {
	for _, code := range []codes.Code{codes.NotFound, codes.FailedPrecondition} {
		t.Run(code.String(), func(t *testing.T) {
			f := &fakeLease{renew: func(int32, *pb.RenewRequest) (*pb.Lease, error) {
				return nil, status.Error(code, "gone")
			}}
			lc, _ := dialFakeLease(t, f)
			failures := lc.KeepAlive(context.Background(), "L1", 40*time.Millisecond)
			select {
			case err := <-failures:
				if status.Code(err) != code {
					t.Fatalf("err = %v", err)
				}
			case <-time.After(2 * time.Second):
				t.Fatal("no failure reported")
			}
			select {
			case _, ok := <-failures:
				if ok {
					t.Fatal("expected channel closed")
				}
			case <-time.After(2 * time.Second):
				t.Fatal("channel not closed")
			}
			if n := f.renews.Load(); n != 1 {
				t.Fatalf("renewed %d times, want 1", n)
			}
		})
	}
}

func TestKeepAliveReportsTransientErrorsAndContinues(t *testing.T) {
	f := &fakeLease{renew: func(n int32, r *pb.RenewRequest) (*pb.Lease, error) {
		if n == 1 {
			return nil, status.Error(codes.Unavailable, "blip")
		}
		return okRenew(n, r)
	}}
	lc, _ := dialFakeLease(t, f)
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	failures := lc.KeepAlive(ctx, "L1", 40*time.Millisecond)
	select {
	case err := <-failures:
		if status.Code(err) != codes.Unavailable {
			t.Fatalf("err = %v", err)
		}
	case <-time.After(2 * time.Second):
		t.Fatal("no failure reported")
	}
	eventually(t, "renewal after transient error", func() bool { return f.renews.Load() >= 3 })
}

func TestNewLeaseClientDoesNotOwnConnection(t *testing.T) {
	addr, _ := startFake(t, func(s *grpc.Server) { pb.RegisterLeaseServiceServer(s, &fakeLease{renew: okRenew}) })
	conn, err := grpc.NewClient(addr, grpc.WithTransportCredentials(insecure.NewCredentials()))
	if err != nil {
		t.Fatal(err)
	}
	defer conn.Close()
	lc := NewLeaseClient(conn)
	if err := lc.Close(); err != nil {
		t.Fatal(err)
	}
	// Connection must still be usable after the LeaseClient is closed.
	if _, err := lc.Grant(context.Background(), "r", time.Second); err != nil {
		t.Fatalf("conn closed by non-owning LeaseClient: %v", err)
	}
}
