package coordin8

import (
	"context"
	"net"
	"sync"
	"testing"
	"time"

	"google.golang.org/grpc"
	"google.golang.org/grpc/metadata"
)

// startFake serves whatever register installs on a loopback listener and
// returns its address. Every call's "authorization" metadata is recorded.
func startFake(t *testing.T, register func(s *grpc.Server)) (string, *authRecorder) {
	t.Helper()
	lis, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	rec := &authRecorder{}
	srv := grpc.NewServer(
		grpc.UnaryInterceptor(func(ctx context.Context, req any, _ *grpc.UnaryServerInfo, h grpc.UnaryHandler) (any, error) {
			rec.record(ctx)
			return h(ctx, req)
		}),
		grpc.StreamInterceptor(func(srv any, ss grpc.ServerStream, _ *grpc.StreamServerInfo, h grpc.StreamHandler) error {
			rec.record(ss.Context())
			return h(srv, ss)
		}),
	)
	register(srv)
	go srv.Serve(lis)
	t.Cleanup(srv.Stop)
	return lis.Addr().String(), rec
}

type authRecorder struct {
	mu   sync.Mutex
	seen []string
}

func (r *authRecorder) record(ctx context.Context) {
	md, _ := metadata.FromIncomingContext(ctx)
	v := md.Get("authorization")
	r.mu.Lock()
	defer r.mu.Unlock()
	if len(v) == 0 {
		r.seen = append(r.seen, "")
	} else {
		r.seen = append(r.seen, v[0])
	}
}

func (r *authRecorder) all() []string {
	r.mu.Lock()
	defer r.mu.Unlock()
	return append([]string(nil), r.seen...)
}

// eventually polls cond for up to 2s.
func eventually(t *testing.T, what string, cond func() bool) {
	t.Helper()
	deadline := time.Now().Add(2 * time.Second)
	for time.Now().Before(deadline) {
		if cond() {
			return
		}
		time.Sleep(5 * time.Millisecond)
	}
	t.Fatalf("timed out waiting for %s", what)
}
