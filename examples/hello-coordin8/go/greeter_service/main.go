// greeter_service registers itself with the Djinn as a Greeter,
// serves Hello requests over gRPC, and keeps its lease alive.
//
// Stop it (Ctrl+C) and the lease expires — the Registry entry disappears.
// The client will get "no matching capability" without any manual cleanup.
package main

import (
	"context"
	"fmt"
	"log"
	"net"
	"os"
	"os/signal"
	"syscall"
	"time"

	gen "github.com/coordin8/examples/hello-coordin8/gen"
	"github.com/coordin8/sdk-go/coordin8"
	"google.golang.org/grpc"
)

const leaseTTL = 30 * time.Second

func envOr(key, def string) string {
	if v := os.Getenv(key); v != "" {
		return v
	}
	return def
}

// greeterServer implements gen.GreeterServiceServer.
type greeterServer struct {
	gen.UnimplementedGreeterServiceServer
}

func (s *greeterServer) Hello(_ context.Context, req *gen.HelloRequest) (*gen.HelloResponse, error) {
	return &gen.HelloResponse{
		Message: fmt.Sprintf("Hello, %s! (from Coordin8 greeter_service)", req.Name),
	}, nil
}

func main() {
	grpcPort     := envOr("GRPC_PORT", "50051")
	djinnHost    := envOr("DJINN_HOST", "localhost")
	advertiseHost := envOr("ADVERTISE_HOST", "localhost")

	// ── 1. Start the gRPC server ──────────────────────────────────────────────
	lis, err := net.Listen("tcp", ":"+grpcPort)
	if err != nil {
		log.Fatalf("listen: %v", err)
	}
	grpcServer := grpc.NewServer()
	gen.RegisterGreeterServiceServer(grpcServer, &greeterServer{})

	go func() {
		if err := grpcServer.Serve(lis); err != nil {
			log.Fatalf("grpc serve: %v", err)
		}
	}()
	fmt.Printf("Greeter gRPC server listening on :%s\n", grpcPort)

	// ── 2. Connect to the Djinn ───────────────────────────────────────────────
	djinn, err := coordin8.Connect(djinnHost)
	if err != nil {
		log.Fatalf("connect to djinn: %v", err)
	}
	defer djinn.Close()

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	// ── 3. Register with the Registry ─────────────────────────────────────────
	// Transport descriptor tells the client how to reach us.
	// The client never hardcodes this — it comes from the Registry.
	reg, err := djinn.Registry().Register(ctx, coordin8.Registration{
		Interface: "Greeter",
		Attrs:     map[string]string{"language": "english"},
		TTL:       leaseTTL,
		Transport: &coordin8.TransportDescriptor{
			Type:   "grpc",
			Config: map[string]string{"host": advertiseHost, "port": grpcPort},
		},
	})
	if err != nil {
		log.Fatalf("register: %v", err)
	}
	fmt.Printf("Registered with Djinn\n")
	fmt.Printf("  interface:  Greeter\n")
	fmt.Printf("  language:   english\n")
	fmt.Printf("  transport:  grpc @ %s:%s\n", advertiseHost, grpcPort)
	fmt.Printf("  lease:      %s (TTL=%s, auto-renewing)\n", reg.LeaseID, leaseTTL)
	fmt.Println("Serving... (Ctrl+C to stop)")

	// ── 4. Keep lease alive in background ─────────────────────────────────────
	go djinn.Leases().KeepAlive(ctx, reg.LeaseID, leaseTTL)

	// ── 5. Wait for shutdown signal ───────────────────────────────────────────
	sigCh := make(chan os.Signal, 1)
	signal.Notify(sigCh, os.Interrupt, syscall.SIGTERM)
	<-sigCh

	fmt.Println("\nShutting down — cancelling registration lease...")
	// Cancel explicitly so the Registry clears immediately (no waiting for TTL)
	_ = djinn.Leases().Cancel(context.Background(), reg.LeaseID)
	grpcServer.GracefulStop()
	fmt.Println("Gone.")
}
