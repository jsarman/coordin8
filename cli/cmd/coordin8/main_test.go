package main

import (
	"bytes"
	"context"
	"io"
	"net"
	"os"
	"strings"
	"sync"
	"testing"
	"time"

	pb "github.com/coordin8/sdk-go/gen/coordin8"
	"github.com/golang-jwt/jwt/v5"
	"github.com/spf13/cobra"
	"github.com/spf13/pflag"
	"google.golang.org/grpc"
	"google.golang.org/grpc/metadata"
	"google.golang.org/protobuf/types/known/emptypb"
	"google.golang.org/protobuf/types/known/timestamppb"
)

// resetGlobals restores every package-level flag variable to its default so
// tests that execute rootCmd in-process don't leak state into each other
// (cobra does not reset bound flag variables between Execute calls).
func resetGlobals(t *testing.T) {
	t.Helper()
	registryAddr, token = "localhost:9002", ""
	leaseGrantor, leaseID, leaseResourceID, leaseTTL = "", "", "", 30
	authSecret, authSub, authScope, authIssuer, authTTL = "", "", "", "", 24*time.Hour
	t.Setenv("COORDIN8_TOKEN", "")
	t.Setenv("COORDIN8_JWT_SECRET", "")
	clearChanged(rootCmd)
	t.Cleanup(func() {
		clearChanged(rootCmd)
		registryAddr, token = "localhost:9002", ""
		leaseGrantor, leaseID, leaseResourceID, leaseTTL = "", "", "", 30
		authSecret, authSub, authScope, authIssuer, authTTL = "", "", "", "", 24*time.Hour
	})
}

// clearChanged forgets which flags were set by a previous Execute; cobra's
// required-flag check keys off Flag.Changed, which otherwise persists.
func clearChanged(c *cobra.Command) {
	reset := func(f *pflag.Flag) { f.Changed = false }
	c.Flags().VisitAll(reset)
	c.PersistentFlags().VisitAll(reset)
	for _, sub := range c.Commands() {
		clearChanged(sub)
	}
}

// runCLI executes the root command in-process with args and returns what it
// wrote to stdout.
func runCLI(t *testing.T, args ...string) (string, error) {
	t.Helper()
	old := os.Stdout
	r, w, err := os.Pipe()
	if err != nil {
		t.Fatal(err)
	}
	os.Stdout = w
	var buf bytes.Buffer
	done := make(chan struct{})
	go func() { io.Copy(&buf, r); close(done) }()

	rootCmd.SetArgs(args)
	rootCmd.SetOut(io.Discard)
	rootCmd.SetErr(io.Discard)
	rootCmd.SilenceUsage, rootCmd.SilenceErrors = true, true
	execErr := rootCmd.Execute()

	w.Close()
	<-done
	os.Stdout = old
	r.Close()
	return buf.String(), execErr
}

// ── effectiveToken ──────────────────────────────────────────────────────────

func TestEffectiveTokenPrecedence(t *testing.T) {
	resetGlobals(t)

	if got := effectiveToken(); got != "" {
		t.Fatalf("no flag, no env: got %q", got)
	}

	t.Setenv("COORDIN8_TOKEN", "from-env")
	if got := effectiveToken(); got != "from-env" {
		t.Fatalf("env only: got %q", got)
	}

	token = "from-flag"
	if got := effectiveToken(); got != "from-flag" {
		t.Fatalf("flag must win over env: got %q", got)
	}
}

// ── dialLeaseGrantor ────────────────────────────────────────────────────────

type fakeLeaseServer struct {
	pb.UnimplementedLeaseServiceServer
	mu    sync.Mutex
	auths []string
	calls []string
}

func (f *fakeLeaseServer) note(ctx context.Context, call string) {
	md, _ := metadata.FromIncomingContext(ctx)
	a := ""
	if v := md.Get("authorization"); len(v) > 0 {
		a = v[0]
	}
	f.mu.Lock()
	f.auths = append(f.auths, a)
	f.calls = append(f.calls, call)
	f.mu.Unlock()
}

func (f *fakeLeaseServer) Renew(ctx context.Context, r *pb.RenewRequest) (*pb.Lease, error) {
	f.note(ctx, "renew:"+r.LeaseId)
	return &pb.Lease{LeaseId: r.LeaseId, ResourceId: "res-1", TtlSeconds: r.TtlSeconds,
		ExpiresAt: timestamppb.New(time.Date(2030, 1, 2, 3, 4, 5, 0, time.UTC))}, nil
}

func (f *fakeLeaseServer) Cancel(ctx context.Context, r *pb.CancelRequest) (*emptypb.Empty, error) {
	f.note(ctx, "cancel:"+r.LeaseId)
	return &emptypb.Empty{}, nil
}

func startLeaseServer(t *testing.T) (string, *fakeLeaseServer) {
	t.Helper()
	lis, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	f := &fakeLeaseServer{}
	srv := grpc.NewServer()
	pb.RegisterLeaseServiceServer(srv, f)
	go srv.Serve(lis)
	t.Cleanup(srv.Stop)
	return lis.Addr().String(), f
}

func (f *fakeLeaseServer) snapshot() (calls, auths []string) {
	f.mu.Lock()
	defer f.mu.Unlock()
	return append([]string(nil), f.calls...), append([]string(nil), f.auths...)
}

func TestDialLeaseGrantorDefaultsToRegistry(t *testing.T) {
	resetGlobals(t)
	addr, f := startLeaseServer(t)
	registryAddr = addr

	lc, err := dialLeaseGrantor()
	if err != nil {
		t.Fatal(err)
	}
	defer lc.Close()
	if err := lc.Cancel(context.Background(), "L1"); err != nil {
		t.Fatalf("lease client should have dialed --registry: %v", err)
	}
	if calls, _ := f.snapshot(); len(calls) != 1 || calls[0] != "cancel:L1" {
		t.Fatalf("calls = %v", calls)
	}
}

func TestDialLeaseGrantorHonorsGrantorFlag(t *testing.T) {
	resetGlobals(t)
	grantorAddr, f := startLeaseServer(t)
	registryAddr = "127.0.0.1:1" // nothing listens here; must not be used
	leaseGrantor = grantorAddr

	lc, err := dialLeaseGrantor()
	if err != nil {
		t.Fatal(err)
	}
	defer lc.Close()
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()
	if err := lc.Cancel(ctx, "L2"); err != nil {
		t.Fatalf("lease client should have dialed --grantor: %v", err)
	}
	if calls, _ := f.snapshot(); len(calls) != 1 {
		t.Fatalf("calls = %v", calls)
	}
}

func TestDialLeaseGrantorAttachesEffectiveToken(t *testing.T) {
	resetGlobals(t)
	addr, f := startLeaseServer(t)
	registryAddr = addr
	t.Setenv("COORDIN8_TOKEN", "env-tok")

	lc, err := dialLeaseGrantor()
	if err != nil {
		t.Fatal(err)
	}
	defer lc.Close()
	if err := lc.Cancel(context.Background(), "L"); err != nil {
		t.Fatal(err)
	}
	if _, auths := f.snapshot(); len(auths) != 1 || auths[0] != "Bearer env-tok" {
		t.Fatalf("auths = %v", auths)
	}
}

func TestDialLeaseGrantorNoTokenNoHeader(t *testing.T) {
	resetGlobals(t)
	addr, f := startLeaseServer(t)
	registryAddr = addr
	lc, err := dialLeaseGrantor()
	if err != nil {
		t.Fatal(err)
	}
	defer lc.Close()
	_ = lc.Cancel(context.Background(), "L")
	if _, auths := f.snapshot(); len(auths) != 1 || auths[0] != "" {
		t.Fatalf("auths = %v", auths)
	}
}

// ── cobra commands in-process ───────────────────────────────────────────────

func TestLeaseRenewCommandPrintsRecord(t *testing.T) {
	resetGlobals(t)
	addr, f := startLeaseServer(t)

	out, err := runCLI(t, "--registry", addr, "--token", "flag-tok", "lease", "renew", "--id", "abc", "--ttl", "60")
	if err != nil {
		t.Fatal(err)
	}
	for _, want := range []string{"lease_id:    abc", "resource_id: res-1", "2030-01-02T03:04:05Z  (extended)"} {
		if !strings.Contains(out, want) {
			t.Errorf("output missing %q:\n%s", want, out)
		}
	}
	calls, auths := f.snapshot()
	if len(calls) != 1 || calls[0] != "renew:abc" || auths[0] != "Bearer flag-tok" {
		t.Fatalf("calls=%v auths=%v", calls, auths)
	}
}

func TestLeaseCancelCommandUsesGrantorFlag(t *testing.T) {
	resetGlobals(t)
	addr, f := startLeaseServer(t)

	out, err := runCLI(t, "--registry", "127.0.0.1:1", "lease", "cancel", "--grantor", addr, "--id", "zzz")
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(out, "cancelled: zzz") {
		t.Fatalf("output = %q", out)
	}
	if calls, _ := f.snapshot(); len(calls) != 1 || calls[0] != "cancel:zzz" {
		t.Fatalf("calls = %v", calls)
	}
}

func TestLeaseRenewRequiresID(t *testing.T) {
	resetGlobals(t)
	// The --id flag is marked required (see init); cobra must reject before dialing.
	if _, err := runCLI(t, "lease", "renew"); err == nil || !strings.Contains(err.Error(), "id") {
		t.Fatalf("err = %v", err)
	}
}

// ── auth mint-token ─────────────────────────────────────────────────────────

func parseMinted(t *testing.T, out, secret string) *authClaims {
	t.Helper()
	claims := &authClaims{}
	tok, err := jwt.ParseWithClaims(strings.TrimSpace(out), claims, func(tk *jwt.Token) (any, error) {
		return []byte(secret), nil
	}, jwt.WithValidMethods([]string{"HS256"}))
	if err != nil || !tok.Valid {
		t.Fatalf("token did not verify: %v", err)
	}
	return claims
}

func TestMintTokenClaims(t *testing.T) {
	resetGlobals(t)
	before := time.Now().Add(-time.Second)
	out, err := runCLI(t, "auth", "mint-token", "--secret", "s3cret", "--sub", "greeter",
		"--ttl", "2h", "--scope", "admin", "--issuer", "coordin8-test")
	if err != nil {
		t.Fatal(err)
	}
	after := time.Now().Add(time.Second)

	c := parseMinted(t, out, "s3cret")
	if c.Subject != "greeter" || c.Scope != "admin" || c.Issuer != "coordin8-test" {
		t.Fatalf("claims = %+v", c)
	}
	iat, exp := c.IssuedAt.Time, c.ExpiresAt.Time
	if iat.Before(before) || iat.After(after) {
		t.Fatalf("iat %v not near now", iat)
	}
	if got := exp.Sub(iat); got != 2*time.Hour {
		t.Fatalf("exp-iat = %v, want 2h", got)
	}
}

func TestMintTokenWrongSecretFailsVerification(t *testing.T) {
	resetGlobals(t)
	out, err := runCLI(t, "auth", "mint-token", "--secret", "right", "--sub", "x")
	if err != nil {
		t.Fatal(err)
	}
	_, err = jwt.ParseWithClaims(strings.TrimSpace(out), &authClaims{}, func(*jwt.Token) (any, error) {
		return []byte("wrong"), nil
	})
	if err == nil {
		t.Fatal("token verified with the wrong secret")
	}
}

func TestMintTokenSecretFromEnv(t *testing.T) {
	resetGlobals(t)
	t.Setenv("COORDIN8_JWT_SECRET", "env-secret")
	out, err := runCLI(t, "auth", "mint-token", "--sub", "svc")
	if err != nil {
		t.Fatal(err)
	}
	c := parseMinted(t, out, "env-secret")
	if c.Subject != "svc" {
		t.Fatalf("sub = %q", c.Subject)
	}
	// Defaults: 24h TTL, scope/issuer omitted.
	if got := c.ExpiresAt.Sub(c.IssuedAt.Time); got != 24*time.Hour {
		t.Fatalf("default ttl = %v", got)
	}
	if c.Scope != "" || c.Issuer != "" {
		t.Fatalf("optional claims should be empty: %+v", c)
	}
}

func TestMintTokenFlagSecretBeatsEnv(t *testing.T) {
	resetGlobals(t)
	t.Setenv("COORDIN8_JWT_SECRET", "env-secret")
	out, err := runCLI(t, "auth", "mint-token", "--secret", "flag-secret", "--sub", "svc")
	if err != nil {
		t.Fatal(err)
	}
	parseMinted(t, out, "flag-secret")
}

func TestMintTokenErrorsWithoutSecret(t *testing.T) {
	resetGlobals(t)
	out, err := runCLI(t, "auth", "mint-token", "--sub", "svc")
	if err == nil || !strings.Contains(err.Error(), "no signing secret") {
		t.Fatalf("err = %v", err)
	}
	if strings.TrimSpace(out) != "" {
		t.Fatalf("nothing should be printed on error, got %q", out)
	}
}

func TestMintTokenRequiresSub(t *testing.T) {
	resetGlobals(t)
	_, err := runCLI(t, "auth", "mint-token", "--secret", "s")
	if err == nil || !strings.Contains(err.Error(), "sub") {
		t.Fatalf("err = %v", err)
	}
}
