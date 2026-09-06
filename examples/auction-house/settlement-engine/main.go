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
	registryAddr := envOr("COORDIN8_REGISTRY", "localhost:9002")

	fmt.Println("Settlement Engine starting...")
	fmt.Printf("  registry: %s\n", registryAddr)

	djinn, err := coordin8.Connect(registryAddr)
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
	// Registration lease was granted by Registry itself, so it's renewed
	// against Registry's own LeaseService (mounted on the same connection).
	keepAliveFailures := djinn.RegistryLeases().KeepAlive(ctx, reg.LeaseID, 30*time.Second)
	go func() {
		for err := range keepAliveFailures {
			log.Printf("lease renewal failed: %v", err)
		}
	}()
	fmt.Printf("  registered: Settlement (lease=%s)\n", reg.LeaseID)

	// Reconcile before going live: catch any auction that already expired while
	// this engine was down. The live Notify watch below only ever sees expiry
	// events that fire while it's connected — a JavaSpaces-style notify(), not a
	// durable one — so without this pass, an auction that expires during an
	// outage would never be settled. See createAuction's "auction-meta" write
	// (durable, TTL=FOREVER) in AuctionService.java for the durable side of this.
	reconcile(ctx, djinn)

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
			settleByAuctionID(ctx, djinn, evt.Tuple.Attrs["auction_id"])
		}
	}()

	fmt.Println("Settlement Engine ready.")

	sigCh := make(chan os.Signal, 1)
	signal.Notify(sigCh, os.Interrupt, syscall.SIGTERM)
	<-sigCh
	fmt.Println("\nShutting down...")
	_ = djinn.RegistryLeases().Cancel(context.Background(), reg.LeaseID)
}

// reconcile scans for auctions whose durable "auction-meta" record (written by
// AuctionService at creation time, TTL=FOREVER) is still present past its
// expires_at — meaning they expired without ever being settled, most likely
// because this engine was down at the time. Runs once at startup, before the
// live Notify watch takes over for the normal case.
func reconcile(ctx context.Context, djinn *coordin8.Client) {
	metas, err := djinn.Space().Contents(ctx, map[string]string{"type": "auction-meta"}, "")
	if err != nil {
		log.Printf("  ! reconcile: failed to scan auction-meta: %v", err)
		return
	}

	now := time.Now().Unix()
	pending := 0
	for _, m := range metas {
		expiresAt, _ := strconv.ParseInt(m.Attrs["expires_at"], 10, 64)
		if expiresAt > now {
			continue // not due yet — the live watch will catch it normally
		}
		pending++
		auctionID := m.Attrs["auction_id"]
		fmt.Printf("  ↻ reconciling auction expired while offline: %s\n", auctionID)
		settleByAuctionID(ctx, djinn, auctionID)
	}
	if pending > 0 {
		fmt.Printf("  reconciliation settled %d auction(s)\n", pending)
	}
}

// settleByAuctionID reconstructs an auction's outcome entirely from durable
// Space state — the permanent "bid" tuples (already written by AuctionService
// for audit purposes) and the durable "auction-meta" tuple — rather than from
// whatever ephemeral payload a live Notify event happened to carry. This is
// what makes settlement correct whether it's triggered by the live watch or by
// reconcile() after a restart: both paths go through the same durable
// reconstruction, so there's exactly one way an auction gets settled.
func settleByAuctionID(ctx context.Context, djinn *coordin8.Client, auctionID string) {
	// Idempotency guard — the live watch and reconcile() could both reach the
	// same auction (e.g. one expires right as the engine restarts).
	if existing, _ := djinn.Space().Read(ctx, coordin8.ReadOpts{
		Template: map[string]string{"type": "sale", "auction_id": auctionID},
	}); existing != nil {
		return
	}

	meta, err := djinn.Space().Read(ctx, coordin8.ReadOpts{
		Template: map[string]string{"type": "auction-meta", "auction_id": auctionID},
	})
	if err != nil || meta == nil {
		log.Printf("  ! settle %s: no durable auction-meta found, skipping", auctionID)
		return
	}

	bids, err := djinn.Space().Contents(ctx, map[string]string{"type": "bid", "auction_id": auctionID}, "")
	if err != nil {
		log.Printf("  ! settle %s: failed to read bid history: %v", auctionID, err)
		return
	}

	var currentBid float64
	var currentBidder string
	for _, b := range bids {
		amount, _ := strconv.ParseFloat(b.Attrs["amount"], 64)
		if amount > currentBid {
			currentBid = amount
			currentBidder = b.Attrs["bidder"]
		}
	}

	item := meta.Attrs["item"]
	reservePrice, _ := strconv.ParseFloat(meta.Attrs["reserve_price"], 64)

	settle(ctx, djinn, auctionID, item, currentBid, currentBidder, reservePrice)

	// Done with the reconciliation record — settled auctions aren't rescanned.
	if err := djinn.Space().CancelTuple(ctx, meta.TupleID); err != nil {
		log.Printf("  ! settle %s: failed to clean up auction-meta: %v", auctionID, err)
	}
}

func settle(ctx context.Context, djinn *coordin8.Client, auctionID, item string, currentBid float64, currentBidder string, reservePrice float64) {
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
