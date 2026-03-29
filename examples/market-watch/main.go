// market-watch — Coordin8 EventMgr live demo
//
// Demonstrates durable event delivery end-to-end:
//   - Subscribe (durable, TTL=60s)
//   - Emit 3 events while no receiver is open (tests mailbox buffering)
//   - Open Receive stream — drains buffered backlog first, then stays live
//   - Emit 2 more events while stream is active (tests live delivery)
//   - Cancel subscription
//
// Run against a live Djinn:
//
//	go run . [djinn-host:port]   (default: localhost:9005)
package main

import (
	"context"
	"fmt"
	"io"
	"log"
	"os"
	"time"

	pb "github.com/coordin8/sdk-go/gen/coordin8"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"
)

func main() {
	addr := "localhost:9005"
	if len(os.Args) > 1 {
		addr = os.Args[1]
	}

	conn, err := grpc.NewClient(addr, grpc.WithTransportCredentials(insecure.NewCredentials()))
	if err != nil {
		log.Fatalf("connect %s: %v", addr, err)
	}
	defer conn.Close()

	client := pb.NewEventServiceClient(conn)
	ctx := context.Background()

	banner("market-watch — Coordin8 EventMgr demo")
	fmt.Printf("  djinn: %s\n\n", addr)

	// ── 1. Subscribe ──────────────────────────────────────────────────────────
	fmt.Println("[1] Subscribing to market.signals (DURABLE, TTL=60s)...")
	reg, err := client.Subscribe(ctx, &pb.SubscribeRequest{
		Source:     "market.signals",
		Template:   map[string]string{},
		Delivery:   pb.DeliveryMode_DURABLE,
		TtlSeconds: 60,
		Handback:   []byte("market-watch"),
	})
	if err != nil {
		log.Fatalf("subscribe: %v", err)
	}
	fmt.Printf("    registration_id: %s\n", reg.RegistrationId)
	fmt.Printf("    lease_id:        %s\n", reg.Lease.LeaseId)
	fmt.Printf("    seq_num at sub:  %d\n\n", reg.SeqNum)

	// ── 2. Emit before anyone is receiving (mailbox test) ─────────────────────
	fmt.Println("[2] Emitting 3 events while receiver is offline (mailbox test)...")
	preload := []struct{ ticker, price string }{
		{"AAPL", "189"}, {"TSLA", "242"}, {"NVDA", "875"},
	}
	for _, e := range preload {
		emit(ctx, client, e.ticker, e.price)
	}
	fmt.Println()

	// ── 3. Open Receive stream — should drain mailbox first ───────────────────
	fmt.Println("[3] Opening Receive stream — expect backlog of 3, then live...")
	stream, err := client.Receive(ctx, &pb.ReceiveRequest{
		RegistrationId: reg.RegistrationId,
	})
	if err != nil {
		log.Fatalf("receive: %v", err)
	}

	received := 0
	done := make(chan struct{})
	go func() {
		defer close(done)
		for {
			ev, err := stream.Recv()
			if err == io.EOF || err != nil {
				return
			}
			received++
			fmt.Printf("    [%d] seq=%-4d  %-15s  %-6s  payload=%s  handback=%s\n",
				received, ev.SeqNum, ev.EventType,
				ev.Attrs["ticker"], string(ev.Payload), string(ev.Handback))
			if received >= 5 {
				return
			}
		}
	}()

	// Let backlog drain, then emit 2 live events
	time.Sleep(200 * time.Millisecond)
	fmt.Println("\n[4] Emitting 2 live events while stream is active...")
	live := []struct{ ticker, price string }{{"MSFT", "415"}, {"GOOGL", "178"}}
	for _, e := range live {
		emit(ctx, client, e.ticker, e.price)
		time.Sleep(100 * time.Millisecond)
	}

	select {
	case <-done:
	case <-time.After(3 * time.Second):
		fmt.Println("    (timeout)")
	}
	fmt.Printf("\n[5] Total received: %d/5\n", received)

	// ── 6. Cancel ─────────────────────────────────────────────────────────────
	fmt.Println("\n[6] Cancelling subscription...")
	_, err = client.CancelSubscription(ctx, &pb.CancelSubscriptionRequest{
		RegistrationId: reg.RegistrationId,
	})
	if err != nil {
		log.Fatalf("cancel: %v", err)
	}
	fmt.Println("    done ✓")

	banner("fin")
}

func emit(ctx context.Context, c pb.EventServiceClient, ticker, price string) {
	_, err := c.Emit(ctx, &pb.EmitRequest{
		Source:    "market.signals",
		EventType: "price.update",
		Attrs:     map[string]string{"ticker": ticker},
		Payload:   []byte(fmt.Sprintf(`{"ticker":"%s","price":%s}`, ticker, price)),
	})
	if err != nil {
		log.Fatalf("emit: %v", err)
	}
	fmt.Printf("    → emitted: %-6s @ $%s\n", ticker, price)
}

func banner(s string) {
	line := "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
	fmt.Printf("\n%s\n  %s\n%s\n", line, s, line)
}
