package coordin8

import (
	"context"
	"strings"
	"testing"
	"time"

	pb "github.com/coordin8/sdk-go/gen/coordin8"
	"google.golang.org/grpc/connectivity"
)

func TestTemplateKeyIsOrderIndependent(t *testing.T) {
	a := templateKey(Template{"interface": "G", "env": "dev", "zone": "a"})
	for i := 0; i < 20; i++ { // map iteration order is randomized
		b := templateKey(Template{"zone": "a", "interface": "G", "env": "dev"})
		if a != b {
			t.Fatalf("%q != %q", a, b)
		}
	}
	if a != "env=dev,interface=G,zone=a" {
		t.Fatalf("key = %q", a)
	}
	if templateKey(Template{"a": "1"}) == templateKey(Template{"a": "2"}) {
		t.Fatal("different values must differ")
	}
	if templateKey(nil) != "" {
		t.Fatal("empty template should yield empty key")
	}
}

func newDiscovery(t *testing.T) (*ServiceDiscovery, *fakeRegistry, *fakeProxy) {
	t.Helper()
	reg, px := newFakeRegistry(), &fakeProxy{}
	c, _ := newRegistryClient(t, reg, px)
	sd := NewServiceDiscovery(c)
	t.Cleanup(sd.Close)
	return sd, reg, px
}

func TestDiscoveryGetCachesConnection(t *testing.T) {
	sd, _, px := newDiscovery(t)
	ctx := context.Background()

	c1, err := sd.Get(ctx, Template{"interface": "G", "env": "dev"})
	if err != nil {
		t.Fatal(err)
	}
	c2, err := sd.Get(ctx, Template{"env": "dev", "interface": "G"})
	if err != nil {
		t.Fatal(err)
	}
	if c1 != c2 {
		t.Fatal("expected the cached connection")
	}
	if px.openCount() != 1 {
		t.Fatalf("opens = %d, want 1", px.openCount())
	}
	// Forwarded port is dialed on the host Proxy itself was dialed at.
	if want := "127.0.0.1:40001"; !strings.HasSuffix(c1.Target(), want) {
		t.Fatalf("target = %q, want suffix %q", c1.Target(), want)
	}

	// A different template opens a second proxy.
	c3, err := sd.Get(ctx, Template{"interface": "Other"})
	if err != nil {
		t.Fatal(err)
	}
	if c3 == c1 || px.openCount() != 2 {
		t.Fatalf("distinct template should open its own proxy (opens=%d)", px.openCount())
	}
}

func TestDiscoveryExpiredMarksStaleAndNextGetReopens(t *testing.T) {
	sd, reg, px := newDiscovery(t)
	ctx := context.Background()
	tmpl := Template{"interface": "G"}

	old, err := sd.Get(ctx, tmpl)
	if err != nil {
		t.Fatal(err)
	}
	eventually(t, "registry watch", func() bool { return reg.watches.Load() >= 1 })

	reg.watchCh <- &pb.RegistryEvent{Type: pb.RegistryEvent_EXPIRED, Capability: &pb.Capability{CapabilityId: "c"}}
	key := templateKey(tmpl)
	eventually(t, "entry stale", func() bool {
		sd.mu.Lock()
		defer sd.mu.Unlock()
		return sd.cache[key].stale
	})
	// Expiry alone must not open anything new.
	if px.openCount() != 1 {
		t.Fatalf("opens = %d", px.openCount())
	}

	fresh, err := sd.Get(ctx, tmpl)
	if err != nil {
		t.Fatal(err)
	}
	if fresh == old {
		t.Fatal("stale entry should be replaced with a new connection")
	}
	if px.openCount() != 2 {
		t.Fatalf("opens = %d, want 2", px.openCount())
	}
	// The caller may still hold the old conn: it must stay open, and its
	// proxy must not be released until Close.
	if got := px.released(); len(got) != 0 {
		t.Fatalf("released = %v, want none", got)
	}
	if old.GetState() == connectivity.Shutdown {
		t.Fatal("old conn was closed while a caller may hold it")
	}
	sd.mu.Lock()
	stale := sd.cache[key].stale
	sd.mu.Unlock()
	if stale {
		t.Fatal("refreshed entry should not be stale")
	}

	sd.Close()
	if got := px.released(); len(got) != 2 {
		t.Fatalf("released after Close = %v, want both proxies", got)
	}
	if old.GetState() != connectivity.Shutdown {
		t.Fatalf("old conn state = %v, want Shutdown after Close", old.GetState())
	}
}

func TestDiscoveryRegisteredAfterExpiredRefreshesEagerly(t *testing.T) {
	sd, reg, px := newDiscovery(t)
	tmpl := Template{"interface": "G"}
	if _, err := sd.Get(context.Background(), tmpl); err != nil {
		t.Fatal(err)
	}
	eventually(t, "registry watch", func() bool { return reg.watches.Load() >= 1 })

	reg.watchCh <- &pb.RegistryEvent{Type: pb.RegistryEvent_EXPIRED}
	reg.watchCh <- &pb.RegistryEvent{Type: pb.RegistryEvent_REGISTERED}
	eventually(t, "eager re-open", func() bool { return px.openCount() == 2 })
	if got := px.released(); len(got) != 0 {
		t.Fatalf("released = %v, old proxy must stay until Close", got)
	}
}

func TestDiscoveryRegisteredWhileFreshDoesNotReopen(t *testing.T) {
	sd, reg, px := newDiscovery(t)
	if _, err := sd.Get(context.Background(), Template{"interface": "G"}); err != nil {
		t.Fatal(err)
	}
	eventually(t, "registry watch", func() bool { return reg.watches.Load() >= 1 })
	reg.watchCh <- &pb.RegistryEvent{Type: pb.RegistryEvent_REGISTERED}
	// Barrier: a following expired event is processed strictly after.
	reg.watchCh <- &pb.RegistryEvent{Type: pb.RegistryEvent_EXPIRED}
	eventually(t, "entry stale", func() bool {
		sd.mu.Lock()
		defer sd.mu.Unlock()
		return sd.cache[templateKey(Template{"interface": "G"})].stale
	})
	if px.openCount() != 1 {
		t.Fatalf("opens = %d, want 1", px.openCount())
	}
}

func TestDiscoveryCloseReleasesAllProxies(t *testing.T) {
	sd, _, px := newDiscovery(t)
	ctx := context.Background()
	a, _ := sd.Get(ctx, Template{"interface": "A"})
	b, _ := sd.Get(ctx, Template{"interface": "B"})
	if a == nil || b == nil {
		t.Fatal("Get failed")
	}

	sd.Close()

	got := px.released()
	if len(got) != 2 {
		t.Fatalf("released = %v", got)
	}
	if !((got[0] == "proxy-1" && got[1] == "proxy-2") || (got[0] == "proxy-2" && got[1] == "proxy-1")) {
		t.Fatalf("released = %v", got)
	}
	if a.GetState() != connectivity.Shutdown || b.GetState() != connectivity.Shutdown {
		t.Fatal("connections should be closed")
	}
	if len(sd.cache) != 0 || len(sd.cancels) != 0 {
		t.Fatal("state not cleared")
	}
	sd.Close() // idempotent: nothing more to release
	if len(px.released()) != 2 {
		t.Fatalf("second Close released more: %v", px.released())
	}
}

func TestDiscoveryGetPropagatesProxyOpenError(t *testing.T) {
	reg := newFakeRegistry()
	// No ProxyService registered on the server: Open fails Unimplemented.
	c, _ := newRegistryClient(t, reg, nil)
	sd := NewServiceDiscovery(c)
	defer sd.Close()
	_, err := sd.Get(context.Background(), Template{"interface": "G"})
	if err == nil || !strings.Contains(err.Error(), "service discovery") {
		t.Fatalf("err = %v", err)
	}
}

// A "modified" event must not close the ClientConn the caller holds, nor
// reopen: Proxy re-resolves per connection so the existing port stays correct.
func TestDiscoveryModifiedDoesNotCloseHeldConnection(t *testing.T) {
	sd, reg, px := newDiscovery(t)
	tmpl := Template{"interface": "G"}
	held, err := sd.Get(context.Background(), tmpl)
	if err != nil {
		t.Fatal(err)
	}
	eventually(t, "registry watch", func() bool { return reg.watches.Load() >= 1 })
	reg.watchCh <- &pb.RegistryEvent{Type: pb.RegistryEvent_MODIFIED}
	// Barrier: the following expired event is processed strictly after.
	reg.watchCh <- &pb.RegistryEvent{Type: pb.RegistryEvent_EXPIRED}
	eventually(t, "entry stale", func() bool {
		sd.mu.Lock()
		defer sd.mu.Unlock()
		return sd.cache[templateKey(tmpl)].stale
	})
	if held.GetState() == connectivity.Shutdown {
		t.Fatal("held connection was closed by a modified event")
	}
	if px.openCount() != 1 || len(px.released()) != 0 {
		t.Fatalf("modified must not reopen/release (opens=%d released=%v)", px.openCount(), px.released())
	}
}

func TestDiscoveryModifiedLeavesFreshEntryUntouched(t *testing.T) {
	sd, reg, px := newDiscovery(t)
	tmpl := Template{"interface": "G"}
	c1, _ := sd.Get(context.Background(), tmpl)
	eventually(t, "registry watch", func() bool { return reg.watches.Load() >= 1 })
	reg.watchCh <- &pb.RegistryEvent{Type: pb.RegistryEvent_MODIFIED}
	reg.watchCh <- &pb.RegistryEvent{Type: pb.RegistryEvent_REGISTERED} // barrier
	time.Sleep(50 * time.Millisecond)
	c2, _ := sd.Get(context.Background(), tmpl)
	if c1 != c2 || px.openCount() != 1 {
		t.Fatalf("modified should keep the cached entry (opens=%d)", px.openCount())
	}
}
