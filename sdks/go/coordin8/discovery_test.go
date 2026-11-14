package coordin8

import (
	"context"
	"strings"
	"sync"
	"testing"
	"time"

	"google.golang.org/grpc"
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

// The service expiring and re-registering changes nothing client side: the
// proxy re-resolves per connection, so the live entry is untouched -- no new
// Open, same conn object, still usable.
func TestDiscoveryRepeatGetKeepsLiveProxy(t *testing.T) {
	sd, _, px := newDiscovery(t)
	tmpl := Template{"interface": "G"}
	held, err := sd.Get(context.Background(), tmpl)
	if err != nil {
		t.Fatal(err)
	}
	time.Sleep(100 * time.Millisecond)

	again, err := sd.Get(context.Background(), tmpl)
	if err != nil {
		t.Fatal(err)
	}
	if again != held {
		t.Fatal("expected the same conn object")
	}
	if held.GetState() == connectivity.Shutdown {
		t.Fatal("held conn was closed")
	}
	if px.openCount() != 1 || len(px.released()) != 0 {
		t.Fatalf("opens=%d released=%v, want 1 / none", px.openCount(), px.released())
	}
}

// Regression: repeated Gets must not accumulate live proxies (each holds a
// Djinn port and renews a lease).
func TestDiscoveryAtMostOneLiveProxyAcrossGets(t *testing.T) {
	sd, _, px := newDiscovery(t)
	tmpl := Template{"interface": "G"}
	held, err := sd.Get(context.Background(), tmpl)
	if err != nil {
		t.Fatal(err)
	}

	for i := 0; i < 6; i++ {
		time.Sleep(30 * time.Millisecond)
		if _, err := sd.Get(context.Background(), tmpl); err != nil {
			t.Fatal(err)
		}
	}
	time.Sleep(100 * time.Millisecond)
	if live := px.openCount() - len(px.released()); live != 1 {
		t.Fatalf("live proxies = %d (opens=%d released=%v), want 1", live, px.openCount(), px.released())
	}
	if held.GetState() == connectivity.Shutdown {
		t.Fatal("held conn was closed")
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
	if len(sd.cache) != 0 {
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

// Concurrent first Gets for one template share a single proxy: none of
// them closes a connection another has already returned.
func TestDiscoveryConcurrentGetsShareOneProxy(t *testing.T) {
	sd, _, px := newDiscovery(t)
	px.openDelay = 100 * time.Millisecond
	tmpl := Template{"interface": "G"}

	const n = 8
	conns := make([]*grpc.ClientConn, n)
	var wg sync.WaitGroup
	for i := 0; i < n; i++ {
		wg.Add(1)
		go func(i int) {
			defer wg.Done()
			c, err := sd.Get(context.Background(), tmpl)
			if err != nil {
				t.Error(err)
			}
			conns[i] = c
		}(i)
	}
	wg.Wait()
	if px.openCount() != 1 {
		t.Fatalf("opens = %d, want 1", px.openCount())
	}
	for i, c := range conns {
		if c != conns[0] || c.GetState() == connectivity.Shutdown {
			t.Fatalf("Get %d got a different or closed conn", i)
		}
	}
}

// Discovery keeps no Registry watch: the proxy re-resolves per connection,
// so a stream per template would be held for nothing.
func TestDiscoveryHoldsNoRegistryWatch(t *testing.T) {
	sd, reg, _ := newDiscovery(t)
	if _, err := sd.Get(context.Background(), Template{"interface": "G"}); err != nil {
		t.Fatal(err)
	}
	time.Sleep(100 * time.Millisecond)
	if n := reg.watches.Load(); n != 0 {
		t.Fatalf("registry watches = %d, want 0", n)
	}
}
