package coordin8

import (
	"context"
	"testing"
	"time"

	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
)

func newProxyClient(t *testing.T, px *fakeProxy) *Client {
	t.Helper()
	c, _ := newRegistryClient(t, newFakeRegistry(), px)
	return c
}

func eventuallyWithin(t *testing.T, d time.Duration, what string, cond func() bool) {
	t.Helper()
	deadline := time.Now().Add(d)
	for time.Now().Before(deadline) {
		if cond() {
			return
		}
		time.Sleep(5 * time.Millisecond)
	}
	t.Fatalf("timed out waiting for %s", what)
}

func TestProxyOpenSendsTTLAndExposesLease(t *testing.T) {
	px := &fakeProxy{}
	c := newProxyClient(t, px)
	h, err := c.Proxy().OpenWithTTL(context.Background(), Template{"interface": "G"}, 45*time.Second)
	if err != nil {
		t.Fatal(err)
	}
	defer h.Release(context.Background())
	if got := px.requestedTTLs(); len(got) != 1 || got[0] != 45 {
		t.Fatalf("requested ttls = %v, want [45]", got)
	}
	if h.Lease.LeaseID != "please-1" {
		t.Fatalf("lease = %+v", h.Lease)
	}

	px2 := &fakeProxy{}
	h2, err := newProxyClient(t, px2).Proxy().Open(context.Background(), Template{"interface": "G"})
	if err != nil {
		t.Fatal(err)
	}
	defer h2.Release(context.Background())
	if got := px2.requestedTTLs(); len(got) != 1 || got[0] != uint64(DefaultProxyTTL.Seconds()) {
		t.Fatalf("default ttls = %v", got)
	}
}

func TestProxyKeepAliveRenewsPeriodicallyUntilRelease(t *testing.T) {
	px := &fakeProxy{}
	c := newProxyClient(t, px)
	h, err := c.Proxy().Open(context.Background(), Template{"interface": "G"})
	if err != nil {
		t.Fatal(err)
	}
	// Granted TTL is 1s => renew every 500ms.
	eventuallyWithin(t, 4*time.Second, "two renewals", func() bool { return px.renewCount() >= 2 })
	px.mu.Lock()
	for _, id := range px.renewIDs {
		if id != "please-1" {
			t.Errorf("renewed %q", id)
		}
	}
	px.mu.Unlock()

	if err := h.Release(context.Background()); err != nil {
		t.Fatal(err)
	}
	time.Sleep(100 * time.Millisecond) // let any in-flight renew land
	n := px.renewCount()
	time.Sleep(1200 * time.Millisecond)
	if got := px.renewCount(); got != n {
		t.Fatalf("renewals continued after Release: %d -> %d", n, got)
	}
	select {
	case <-h.Lost():
		t.Fatal("Release must not report the lease as lost")
	default:
	}
}

func TestProxyRenewNotFoundReportsLost(t *testing.T) {
	px := &fakeProxy{renewErr: func(int) error { return status.Error(codes.NotFound, "gone") }}
	c := newProxyClient(t, px)
	h, err := c.Proxy().Open(context.Background(), Template{"interface": "G"})
	if err != nil {
		t.Fatal(err)
	}
	defer h.Release(context.Background())
	select {
	case <-h.Lost():
	case <-time.After(3 * time.Second):
		t.Fatal("Lost() never closed")
	}
}

func TestProxyTransientRenewErrorKeepsRenewing(t *testing.T) {
	px := &fakeProxy{renewErr: func(n int) error {
		if n <= 2 {
			return status.Error(codes.Unavailable, "blip")
		}
		return nil
	}}
	c := newProxyClient(t, px)
	h, err := c.Proxy().Open(context.Background(), Template{"interface": "G"})
	if err != nil {
		t.Fatal(err)
	}
	defer h.Release(context.Background())
	eventuallyWithin(t, 5*time.Second, "renewals past transient errors", func() bool { return px.renewCount() >= 4 })
	select {
	case <-h.Lost():
		t.Fatal("transient error must not mark the lease lost")
	default:
	}
}

func TestDiscoveryLostLeaseMarksStaleAndNextGetReopens(t *testing.T) {
	reg, px := newFakeRegistry(), &fakeProxy{}
	px.renewErr = func(int) error { return status.Error(codes.FailedPrecondition, "expired") }
	c, _ := newRegistryClient(t, reg, px)
	sd := NewServiceDiscovery(c)
	t.Cleanup(sd.Close)
	tmpl := Template{"interface": "G"}
	key := templateKey(tmpl)

	old, err := sd.Get(context.Background(), tmpl)
	if err != nil {
		t.Fatal(err)
	}
	eventuallyWithin(t, 3*time.Second, "entry stale after lost lease", func() bool {
		sd.mu.Lock()
		defer sd.mu.Unlock()
		return sd.cache[key].stale
	})
	if px.openCount() != 1 {
		t.Fatalf("opens = %d", px.openCount())
	}
	fresh, err := sd.Get(context.Background(), tmpl)
	if err != nil {
		t.Fatal(err)
	}
	if fresh == old || px.openCount() != 2 {
		t.Fatalf("expected a new proxy (opens=%d)", px.openCount())
	}
	// The lost proxy's conn is dead anyway and is torn down on replacement.
	if got := px.released(); len(got) != 1 || got[0] != "proxy-1" {
		t.Fatalf("released = %v", got)
	}
}
