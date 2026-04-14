// settlement-engine watches for expired auctions and settles them.
//
// When an auction's lease expires, the engine:
//   - Reads the expired auction to get the winning bid
//   - Checks if the reserve price was met
//   - Writes a sale tuple (permanent record)
//   - Emits a settlement event through EventMgr
//
// "Absence is a signal" — lease expiry IS the auction close event.
package main

import (
	"context"
	"fmt"
	"log"
	"os"
	"os/signal"
	"strconv"
	"syscall"
	"time"

	"github.com/coordin8/sdk-go/coordin8"
)

func envOr(key, def string) string {
	if v := os.Getenv(key); v != "" {
		return v
	}
	return def
}

func main() {
	djinnHost := envOr("DJINN_HOST", "localhost")

	fmt.Println("Settlement Engine starting...")
	fmt.Printf("  djinn: %s\n", djinnHost)

	djinn, err := coordin8.Connect(djinnHost)
	if err != nil {
		log.Fatalf("connect: %v", err)
	}
	defer djinn.Close()

	// Register as Settlement in the Registry
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	reg, err := djinn.Registry().Register(ctx, coordin8.Registration{
		Interface: "Settlement",
		Attrs:     map[string]string{"version": "1.0"},
		TTL:       30 * time.Second,
	})
	if err != nil {
		log.Fatalf("register: %v", err)
	}
	go djinn.Leases().KeepAlive(ctx, reg.LeaseID, 30*time.Second)
	fmt.Printf("  registered: Settlement (lease=%s)\n", reg.LeaseID)

	// Watch for auction expiry
	fmt.Println("  watching for expired auctions...")
	ch, err := djinn.Space().Notify(ctx, coordin8.NotifyOpts{
		Template: map[string]string{"type": "auction"},
		On:       coordin8.Expiration,
		TTL:      300 * time.Second,
	})
	if err != nil {
		log.Fatalf("notify: %v", err)
	}

	go func() {
		for evt := range ch {
			if evt.Tuple == nil {
				continue
			}
			settle(ctx, djinn, evt.Tuple)
		}
	}()

	fmt.Println("Settlement Engine ready.")

	sigCh := make(chan os.Signal, 1)
	signal.Notify(sigCh, os.Interrupt, syscall.SIGTERM)
	<-sigCh
	fmt.Println("\nShutting down...")
	_ = djinn.Leases().Cancel(context.Background(), reg.LeaseID)
}

func settle(ctx context.Context, djinn *coordin8.Client, auction *coordin8.TupleRecord) {
	auctionID := auction.Attrs["auction_id"]
	item := auction.Attrs["item"]
	currentBid, _ := strconv.ParseFloat(auction.Attrs["current_bid"], 64)
	reservePrice, _ := strconv.ParseFloat(auction.Attrs["reserve_price"], 64)
	currentBidder := auction.Attrs["current_bidder"]

	fmt.Printf("\n  ⏰ Auction expired: %s (%s)\n", item, auctionID)

	// Determine outcome
	status := "sold"
	if currentBidder == "" {
		status = "no-bids"
	} else if currentBid < reservePrice {
		status = "reserve-not-met"
	}

	// Write sale record (permanent — TTL 0 means FOREVER)
	sale, err := djinn.Space().Write(ctx, coordin8.WriteOpts{
		Attrs: map[string]string{
			"type":       "sale",
			"auction_id": auctionID,
			"item":       item,
			"status":     status,
			"winner":     currentBidder,
			"price":      fmt.Sprintf("%.2f", currentBid),
			"settled_at": time.Now().Format(time.RFC3339),
		},
		TTL:       0, // permanent
		WrittenBy: "settlement-engine",
	})
	if err != nil {
		log.Printf("  ✗ failed to write sale: %v", err)
		return
	}

	// Emit settlement event
	_ = djinn.Events().Emit(ctx, "auction.events", "auction.settled",
		map[string]string{
			"auction_id": auctionID,
			"status":     status,
			"item":       item,
		}, nil)

	switch status {
	case "sold":
		fmt.Printf("  ✓ SOLD: %s to %s for $%.2f (sale=%s)\n", item, currentBidder, currentBid, sale.TupleID)
	case "reserve-not-met":
		fmt.Printf("  ✗ Reserve not met: %s (bid $%.2f < reserve $%.2f)\n", item, currentBid, reservePrice)
	case "no-bids":
		fmt.Printf("  ✗ No bids: %s\n", item)
	}
}
