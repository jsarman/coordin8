package coordin8

import (
	"context"
	"strings"
	"testing"
	"time"

	pb "github.com/coordin8/sdk-go/gen/coordin8"
	"google.golang.org/grpc"
)

func transportCap(iface, host, port string) *pb.Capability {
	return &pb.Capability{Interface: iface, Transport: &pb.TransportDescriptor{
		Type: "grpc", Config: map[string]string{"host": host, "port": port}}}
}

func TestConnectResolvesServicesThroughRegistry(t *testing.T) {
	reg := newFakeRegistry()
	reg.caps["Proxy"] = transportCap("Proxy", "proxy.host", "9003")
	reg.caps["Space"] = transportCap("Space", "space.host", "9006")
	reg.caps["EventMgr"] = transportCap("EventMgr", "event.host", "9005")
	addr, _ := startFake(t, func(s *grpc.Server) { pb.RegisterRegistryServiceServer(s, reg) })

	c, err := Connect(addr)
	if err != nil {
		t.Fatal(err)
	}
	defer c.Close()

	// grpc.NewClient is lazy, so the resolved targets are observable
	// without anything listening there.
	for name, want := range map[string]string{
		"proxy": "proxy.host:9003", "space": "space.host:9006", "event": "event.host:9005",
	} {
		var got string
		switch name {
		case "proxy":
			got = c.proxyConn.Target()
		case "space":
			got = c.spaceConn.Target()
		case "event":
			got = c.eventConn.Target()
		}
		if !strings.HasSuffix(got, want) {
			t.Errorf("%s target = %q, want suffix %q", name, got, want)
		}
	}
	if len(reg.lookups) != 3 {
		t.Fatalf("expected 3 registry lookups, got %d", len(reg.lookups))
	}
}

func TestConnectPinnedAddrsSkipRegistryLookup(t *testing.T) {
	reg := newFakeRegistry() // empty: any lookup would fail with NotFound
	c, _ := newRegistryClient(t, reg, nil)
	if len(reg.lookups) != 0 {
		t.Fatalf("pinned addresses should not trigger lookups, got %v", reg.lookups)
	}
	_ = c
}

func TestConnectPartiallyPinnedLooksUpOnlyTheRest(t *testing.T) {
	reg := newFakeRegistry()
	reg.caps["EventMgr"] = transportCap("EventMgr", "e", "1")
	addr, _ := startFake(t, func(s *grpc.Server) { pb.RegisterRegistryServiceServer(s, reg) })
	c, err := Connect(addr, WithProxyAddr("p:1"), WithSpaceAddr("s:1"))
	if err != nil {
		t.Fatal(err)
	}
	defer c.Close()
	if len(reg.lookups) != 1 || reg.lookups[0]["interface"] != "EventMgr" {
		t.Fatalf("lookups = %v", reg.lookups)
	}
}

func TestConnectErrorsWhenServiceNotRegistered(t *testing.T) {
	reg := newFakeRegistry()
	addr, _ := startFake(t, func(s *grpc.Server) { pb.RegisterRegistryServiceServer(s, reg) })
	_, err := Connect(addr)
	if err == nil || !strings.Contains(err.Error(), "look up Proxy") {
		t.Fatalf("err = %v", err)
	}
}

func TestLookupAddrValidatesTransport(t *testing.T) {
	cases := map[string]*pb.Capability{
		"no transport": {Interface: "Proxy"},
		"missing port": transportCap("Proxy", "h", ""),
		"missing host": transportCap("Proxy", "", "1"),
	}
	for name, cp := range cases {
		t.Run(name, func(t *testing.T) {
			reg := newFakeRegistry()
			reg.caps["Proxy"] = cp
			addr, _ := startFake(t, func(s *grpc.Server) { pb.RegisterRegistryServiceServer(s, reg) })
			_, err := Connect(addr)
			if err == nil || !strings.Contains(err.Error(), "look up Proxy") {
				t.Fatalf("err = %v", err)
			}
		})
	}
}

func TestWithTokenAttachesBearerToRegistryCalls(t *testing.T) {
	reg := newFakeRegistry()
	reg.caps["Proxy"] = transportCap("Proxy", "p", "1")
	reg.caps["Space"] = transportCap("Space", "s", "1")
	reg.caps["EventMgr"] = transportCap("EventMgr", "e", "1")
	addr, rec := startFake(t, func(s *grpc.Server) { pb.RegisterRegistryServiceServer(s, reg) })

	c, err := Connect(addr, WithToken("sekret"))
	if err != nil {
		t.Fatal(err)
	}
	defer c.Close()
	// The three bootstrap lookups must already be authenticated.
	got := rec.all()
	if len(got) != 3 {
		t.Fatalf("recorded %v", got)
	}
	for _, a := range got {
		if a != "Bearer sekret" {
			t.Fatalf("auth = %q", a)
		}
	}
}

func TestWithoutTokenNoAuthorizationHeader(t *testing.T) {
	reg := newFakeRegistry()
	reg.all = []*pb.Capability{{CapabilityId: "x"}}
	c, rec := newRegistryClient(t, reg, nil)
	if _, err := c.Registry().LookupAll(context.Background(), Template{}); err != nil {
		t.Fatal(err)
	}
	for _, a := range rec.all() {
		if a != "" {
			t.Fatalf("unexpected auth %q", a)
		}
	}
}

func TestPerRPCTokenOptionAndRegistryLeases(t *testing.T) {
	// WithToken covers the lease client built from the Registry connection too.
	reg := newFakeRegistry()
	fl := &fakeLease{renew: okRenew}
	addr, rec := startFake(t, func(s *grpc.Server) {
		pb.RegisterRegistryServiceServer(s, reg)
		pb.RegisterLeaseServiceServer(s, fl)
	})
	c, err := Connect(addr, WithProxyAddr(addr), WithSpaceAddr(addr), WithEventAddr(addr), WithToken("t"))
	if err != nil {
		t.Fatal(err)
	}
	defer c.Close()
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()
	if _, err := c.RegistryLeases().Renew(ctx, "L", time.Second); err != nil {
		t.Fatal(err)
	}
	got := rec.all()
	if len(got) != 1 || got[0] != "Bearer t" {
		t.Fatalf("auth = %v", got)
	}
	if err := c.RegistryLeases().Close(); err != nil {
		t.Fatal(err)
	}
}

func TestBearerCredentialsShape(t *testing.T) {
	md, err := bearerTokenCredentials{token: "abc"}.GetRequestMetadata(context.Background())
	if err != nil || md["authorization"] != "Bearer abc" {
		t.Fatalf("md=%v err=%v", md, err)
	}
	if (bearerTokenCredentials{}).RequireTransportSecurity() {
		t.Fatal("must not require TLS")
	}
	if PerRPCToken("x") == nil {
		t.Fatal("nil dial option")
	}
}
