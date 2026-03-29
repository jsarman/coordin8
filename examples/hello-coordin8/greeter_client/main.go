// greeter_client finds a Greeter in the Djinn and calls Hello.
// No host, port, or transport descriptor in this file — the proxy handles it.
package main

import (
	"context"
	"fmt"
	"log"
	"os"

	gen "github.com/coordin8/examples/hello-coordin8/gen"
	"github.com/coordin8/sdk-go/coordin8"
)

func main() {
	name := "Coordin8"
	if len(os.Args) > 1 {
		name = os.Args[1]
	}

	djinn, err := coordin8.Connect("localhost")
	if err != nil {
		log.Fatalf("connect to djinn: %v", err)
	}
	defer djinn.Close()

	// One call. The Djinn opens a local forwarding port and returns a conn.
	fmt.Println("Opening proxy to Greeter...")
	conn, cleanup, err := djinn.Proxy().ProxyConn(
		context.Background(),
		coordin8.Template{"interface": "Greeter"},
	)
	if err != nil {
		fmt.Printf("  ✗ Proxy failed: %v\n", err)
		os.Exit(1)
	}
	defer cleanup()

	client := gen.NewGreeterServiceClient(conn)
	resp, err := client.Hello(context.Background(), &gen.HelloRequest{Name: name})
	if err != nil {
		log.Fatalf("hello: %v", err)
	}

	fmt.Printf("\n  → %s\n", resp.Message)
}
