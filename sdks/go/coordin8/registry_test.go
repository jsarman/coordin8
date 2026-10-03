package coordin8

import (
	"context"
	"fmt"
	"reflect"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	pb "github.com/coordin8/sdk-go/gen/coordin8"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
	"google.golang.org/protobuf/types/known/emptypb"
)

// fakeRegistry implements RegistryService. Watch streams whatever is sent on
// watchCh until the client goes away.
type fakeRegistry struct {
	pb.UnimplementedRegistryServiceServer

	mu         sync.Mutex
	registered []*pb.RegisterRequest
	lookups    []map[string]string
	caps       map[string]*pb.Capability // by interface
	all        []*pb.Capability
	watchCh    chan *pb.RegistryEvent
	watches    atomic.Int32
}

func newFakeRegistry() *fakeRegistry {
	return &fakeRegistry{caps: map[string]*pb.Capability{}, watchCh: make(chan *pb.RegistryEvent, 16)}
}

func (f *fakeRegistry) Register(_ context.Context, r *pb.RegisterRequest) (*pb.RegisterResponse, error) {
	f.mu.Lock()
	f.registered = append(f.registered, r)
	f.mu.Unlock()
	id := r.CapabilityId
	if id == "" {
		id = "cap-new"
	}
	return &pb.RegisterResponse{CapabilityId: id, Lease: &pb.Lease{LeaseId: "lease-" + id}}, nil
}

func (f *fakeRegistry) Lookup(_ context.Context, r *pb.LookupRequest) (*pb.Capability, error) {
	f.mu.Lock()
	f.lookups = append(f.lookups, r.Template)
	c, ok := f.caps[r.Template["interface"]]
	f.mu.Unlock()
	if !ok {
		return nil, status.Error(codes.NotFound, "no match")
	}
	return c, nil
}

func (f *fakeRegistry) LookupAll(_ *pb.LookupRequest, s pb.RegistryService_LookupAllServer) error {
	for _, c := range f.all {
		if err := s.Send(c); err != nil {
			return err
		}
	}
	return nil
}

func (f *fakeRegistry) Watch(_ *pb.RegistryWatchRequest, s pb.RegistryService_WatchServer) error {
	f.watches.Add(1)
	for {
		select {
		case e := <-f.watchCh:
			if err := s.Send(e); err != nil {
				return err
			}
		case <-s.Context().Done():
			return nil
		}
	}
}

// fakeProxy implements ProxyService, handing out sequential proxy IDs.
type fakeProxy struct {
	pb.UnimplementedProxyServiceServer
	mu       sync.Mutex
	opens    []map[string]string
	releases []string
}

func (f *fakeProxy) Open(_ context.Context, r *pb.OpenRequest) (*pb.ProxyHandle, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.opens = append(f.opens, r.Template)
	n := len(f.opens)
	return &pb.ProxyHandle{ProxyId: fmt.Sprintf("proxy-%d", n), LocalPort: int32(40000 + n)}, nil
}

func (f *fakeProxy) Release(_ context.Context, r *pb.ReleaseRequest) (*emptypb.Empty, error) {
	f.mu.Lock()
	f.releases = append(f.releases, r.ProxyId)
	f.mu.Unlock()
	return &emptypb.Empty{}, nil
}

func (f *fakeProxy) openCount() int {
	f.mu.Lock()
	defer f.mu.Unlock()
	return len(f.opens)
}

func (f *fakeProxy) released() []string {
	f.mu.Lock()
	defer f.mu.Unlock()
	return append([]string(nil), f.releases...)
}

// newRegistryClient serves reg on a fake and returns a connected Client with
// all other services pinned to the same address.
func newRegistryClient(t *testing.T, reg *fakeRegistry, px *fakeProxy, opts ...ConnectOption) (*Client, *authRecorder) {
	t.Helper()
	addr, rec := startFake(t, func(s *grpc.Server) {
		pb.RegisterRegistryServiceServer(s, reg)
		if px != nil {
			pb.RegisterProxyServiceServer(s, px)
		}
	})
	opts = append([]ConnectOption{WithProxyAddr(addr), WithSpaceAddr(addr), WithEventAddr(addr)}, opts...)
	c, err := Connect(addr, opts...)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { c.Close() })
	return c, rec
}

func TestRegisterMapsRequestAndResult(t *testing.T) {
	reg := newFakeRegistry()
	c, _ := newRegistryClient(t, reg, nil)

	res, err := c.Registry().Register(context.Background(), Registration{
		Interface: "Greeter",
		Attrs:     map[string]string{"env": "dev"},
		TTL:       45 * time.Second,
		Transport: &TransportDescriptor{Type: "grpc", Config: map[string]string{"host": "h", "port": "1"}},
	})
	if err != nil {
		t.Fatal(err)
	}
	if res.CapabilityID != "cap-new" || res.LeaseID != "lease-cap-new" {
		t.Fatalf("result = %+v", res)
	}
	got := reg.registered[0]
	if got.Interface != "Greeter" || got.TtlSeconds != 45 || got.CapabilityId != "" ||
		got.Attrs["env"] != "dev" || got.Transport.Type != "grpc" || got.Transport.Config["port"] != "1" {
		t.Fatalf("request = %v", got)
	}
}

func TestRegisterWithCapabilityIDReRegisters(t *testing.T) {
	reg := newFakeRegistry()
	c, _ := newRegistryClient(t, reg, nil)

	res, err := c.Registry().Register(context.Background(), Registration{Interface: "G", TTL: time.Second, CapabilityID: "cap-7"})
	if err != nil {
		t.Fatal(err)
	}
	if reg.registered[0].CapabilityId != "cap-7" || res.CapabilityID != "cap-7" {
		t.Fatalf("capability id not propagated: req=%q res=%q", reg.registered[0].CapabilityId, res.CapabilityID)
	}
	if reg.registered[0].Transport != nil {
		t.Fatal("nil Transport should not produce a transport message")
	}
}

func TestLookupMapsCapability(t *testing.T) {
	reg := newFakeRegistry()
	reg.caps["Greeter"] = &pb.Capability{CapabilityId: "c1", Interface: "Greeter",
		Attrs:     map[string]string{"a": "b"},
		Transport: &pb.TransportDescriptor{Type: "grpc", Config: map[string]string{"host": "x"}}}
	c, _ := newRegistryClient(t, reg, nil)

	cp, err := c.Registry().Lookup(context.Background(), Template{"interface": "Greeter", "a": "contains:b"})
	if err != nil {
		t.Fatal(err)
	}
	want := Capability{CapabilityID: "c1", Interface: "Greeter", Attrs: map[string]string{"a": "b"},
		Transport: &TransportDescriptor{Type: "grpc", Config: map[string]string{"host": "x"}}}
	if !reflect.DeepEqual(cp, want) {
		t.Fatalf("got %+v want %+v", cp, want)
	}
	if reg.lookups[0]["a"] != "contains:b" {
		t.Fatalf("template not forwarded: %v", reg.lookups[0])
	}
}

func TestLookupNotFoundSurfacesStatus(t *testing.T) {
	c, _ := newRegistryClient(t, newFakeRegistry(), nil)
	_, err := c.Registry().Lookup(context.Background(), Template{"interface": "Nope"})
	if status.Code(err) != codes.NotFound {
		t.Fatalf("err = %v", err)
	}
}

func TestLookupAllCollectsStream(t *testing.T) {
	reg := newFakeRegistry()
	reg.all = []*pb.Capability{{CapabilityId: "1"}, {CapabilityId: "2"}, {CapabilityId: "3"}}
	c, _ := newRegistryClient(t, reg, nil)

	caps, err := c.Registry().LookupAll(context.Background(), Template{})
	if err != nil {
		t.Fatal(err)
	}
	if len(caps) != 3 || caps[0].CapabilityID != "1" || caps[2].CapabilityID != "3" {
		t.Fatalf("caps = %+v", caps)
	}
	if caps[0].Transport != nil {
		t.Fatal("missing transport should map to nil")
	}
}

func TestLookupAllEmpty(t *testing.T) {
	c, _ := newRegistryClient(t, newFakeRegistry(), nil)
	caps, err := c.Registry().LookupAll(context.Background(), Template{})
	if err != nil || len(caps) != 0 {
		t.Fatalf("caps=%v err=%v", caps, err)
	}
}

func TestWatchMapsEventTypes(t *testing.T) {
	reg := newFakeRegistry()
	c, _ := newRegistryClient(t, reg, nil)
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	ch, err := c.Registry().Watch(ctx, Template{"interface": "G"})
	if err != nil {
		t.Fatal(err)
	}
	reg.watchCh <- &pb.RegistryEvent{Type: pb.RegistryEvent_REGISTERED, Capability: &pb.Capability{CapabilityId: "a"}}
	reg.watchCh <- &pb.RegistryEvent{Type: pb.RegistryEvent_EXPIRED, Capability: &pb.Capability{CapabilityId: "b"}}
	reg.watchCh <- &pb.RegistryEvent{Type: pb.RegistryEvent_MODIFIED, Capability: &pb.Capability{CapabilityId: "c"}}
	reg.watchCh <- &pb.RegistryEvent{Type: pb.RegistryEvent_EXPIRED} // no capability

	want := []struct{ typ, id string }{{"registered", "a"}, {"expired", "b"}, {"modified", "c"}, {"expired", ""}}
	for _, w := range want {
		select {
		case e := <-ch:
			if e.Type != w.typ || e.Capability.CapabilityID != w.id {
				t.Fatalf("got %s/%s want %s/%s", e.Type, e.Capability.CapabilityID, w.typ, w.id)
			}
		case <-time.After(2 * time.Second):
			t.Fatalf("timeout waiting for %s", w.typ)
		}
	}
	cancel()
	select {
	case _, ok := <-ch:
		if ok {
			t.Fatal("unexpected extra event")
		}
	case <-time.After(2 * time.Second):
		t.Fatal("channel not closed after cancel")
	}
}
