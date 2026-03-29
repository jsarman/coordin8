// greeter_client finds a Greeter in the Registry by template and calls Hello.
// No hardcoded host, port, or address anywhere in this file.
// The Djinn resolves everything.
package main

import (
	"context"
	"fmt"
	"log"
	"os"

	gen "github.com/coordin8/examples/hello-coordin8/gen"
	"github.com/coordin8/sdk-go/coordin8"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"
)

func main() {
	name := "Coordin8"
	if len(os.Args) > 1 {
		name = os.Args[1]
	}

	// ── 1. Connect to the Djinn ───────────────────────────────────────────────
	djinn, err := coordin8.Connect("localhost")
	if err != nil {
		log.Fatalf("connect to djinn: %v", err)
	}
	defer djinn.Close()

	// ── 2. Look up a Greeter — no address, just a template ───────────────────
	fmt.Println("Looking up Greeter in Registry...")
	cap, err := djinn.Registry().Lookup(context.Background(), coordin8.Template{
		"interface": "Greeter",
	})
	if err != nil {
		fmt.Printf("  ✗ No Greeter available: %v\n", err)
		os.Exit(1)
	}

	fmt.Printf("  ✓ Found: %s  (capability=%s)\n", cap.Interface, cap.CapabilityID)
	if cap.Transport != nil {
		fmt.Printf("  ✓ Transport: %s @ %s:%s\n",
			cap.Transport.Type,
			cap.Transport.Config["host"],
			cap.Transport.Config["port"],
		)
	}

	// ── 3. Connect directly using the transport descriptor ───────────────────
	// The host:port came from the Registry — not from this file.
	host := cap.Transport.Config["host"]
	port := cap.Transport.Config["port"]

	conn, err := grpc.NewClient(
		host+":"+port,
		grpc.WithTransportCredentials(insecure.NewCredentials()),
	)
	if err != nil {
		log.Fatalf("dial greeter: %v", err)
	}
	defer conn.Close()

	// ── 4. Call Hello ─────────────────────────────────────────────────────────
	client := gen.NewGreeterServiceClient(conn)
	resp, err := client.Hello(context.Background(), &gen.HelloRequest{Name: name})
	if err != nil {
		log.Fatalf("hello: %v", err)
	}

	fmt.Printf("\n  → %s\n", resp.Message)
}
