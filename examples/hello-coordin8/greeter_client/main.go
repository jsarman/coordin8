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
		log.Fatalf("connect: %v", err)
	}
	defer djinn.Close()

	discovery := coordin8.NewServiceDiscovery(djinn)
	defer discovery.Close()

	conn, err := discovery.Get(context.Background(), coordin8.Template{"interface": "Greeter"})
	if err != nil {
		log.Fatalf("discover: %v", err)
	}

	resp, err := gen.NewGreeterServiceClient(conn).Hello(
		context.Background(),
		&gen.HelloRequest{Name: name},
	)
	if err != nil {
		log.Fatalf("hello: %v", err)
	}

	fmt.Println(resp.Message)
}
