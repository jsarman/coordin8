# Auction House — Coordin8 Polyglot Demo

> Three services. Three languages. One coordination plane.
> No REST calls between them. No message broker. No service mesh. Just tuples.

## What's Running

| Service | Language | Role |
|---------|----------|------|
| **Auction Service** | Java | Creates auctions, validates bids. REST API → Space tuples. |
| **Settlement Engine** | Go | Watches for expired auctions, settles them atomically. |
| **Auction Board** | Node/TypeScript | Browser UI. Watches Space via SSE. No polling. |

None of these services know about each other. They coordinate through the Djinn's tuple Space.

## Quick Start

```bash
cd examples/auction-house
docker compose up --build
```

Open http://localhost:3000 — the Auction Board.

## Demo Walkthrough

### 1. Create an auction

Use the form at the top of the board, or curl:

```bash
curl -X POST http://localhost:8080/auction \
  -H "Content-Type: application/json" \
  -d '{"item": "Vintage Typewriter", "starting_price": 25, "reserve_price": 50, "duration_seconds": 30}'
```

The auction card appears instantly on the board with a countdown timer.

### 2. Place bids

```bash
curl -X POST http://localhost:3000/bid \
  -H "Content-Type: application/json" \
  -d '{"auction_id": "<id>", "bidder": "Alice", "amount": 50}'

curl -X POST http://localhost:3000/bid \
  -H "Content-Type: application/json" \
  -d '{"auction_id": "<id>", "bidder": "Bob", "amount": 75}'
```

Bids appear in real time on the board. Current bid and bidder update instantly.

### 3. Watch the timer expire

No action needed. When the countdown hits zero:
- The auction tuple's lease expires
- The Settlement Engine detects the expiry (absence is a signal)
- It writes a sale tuple and emits a settlement event
- The board shows "SOLD to Bob for $75.00"

### 4. Verify the audit trail

```bash
coordin8 space contents --match type=sale
```

### 5. Kill the settlement engine, create another auction, restart

```bash
docker compose stop settlement-engine
# Create an auction, let it expire...
docker compose start settlement-engine
# Settlement happens on restart — expiry events are durable
```

## How It Works

The key insight: **the auction timer IS a lease TTL**. There's no cron job, no scheduler, no timer service. The lease expires and that IS the close event.

```
Auction Service                  Djinn Space                Settlement Engine
     │                               │                           │
     │  write(auction, TTL=30s)       │                           │
     │──────────────────────────────►│                           │
     │                               │  notify(auction, expire)  │
     │                               │◄──────────────────────────│
     │  write(bid)                   │                           │
     │──────────────────────────────►│                           │
     │                               │                           │
     │                    lease expires                          │
     │                               │  ← expiry event →        │
     │                               │─────────────────────────►│
     │                               │                    write(sale)
     │                               │◄──────────────────────────│
     │                               │                    emit(settled)
```

## Architecture Notes

- **No REST between services** — all coordination goes through Space tuples
- **No service mesh** — services discover the Djinn, not each other
- **Auction Board watches Space directly** — SSE pushes are driven by tuple appearance/expiry events
- **Settlement is reactive** — it doesn't poll, it watches. When a lease expires, it reacts.
