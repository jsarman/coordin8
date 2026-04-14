# Auction House — Polyglot Demo PRD

> Three services. Three languages. One coordination plane.
> No REST calls between them. No message broker. No service mesh. Just tuples.

---

## Purpose

Demonstrate Coordin8's value proposition with a visible, interactive system that non-trivially exercises every core Djinn service. The audience should understand within 60 seconds why tuple-space coordination matters — and see it working across languages in real time.

**This is the "hello world" that sells the project.** The greeter example proves the plumbing works. The auction house proves the *model* works.

---

## What It Demonstrates

| Coordin8 Concept | How the Auction Shows It |
|---|---|
| Space (tuples) | Auctions, bids, and sales are tuples — not REST payloads, not Kafka messages |
| Lease as coordination | Auction duration IS a lease TTL. Timer expires → auction ends. No cron, no scheduler. |
| Absence is a signal | Lease expiry triggers settlement. The thing that *didn't happen* (renewal) closes the auction. |
| watch() / reactivity | UI updates are driven by Space watches. No polling, no webhooks. |
| take() atomicity | Settlement engine claims the winning bid atomically. No double-sells. |
| Transactions (2PC) | Bid claim + sale record commit together or not at all. |
| Registry discovery | Services find each other by capability, not by address. |
| EventMgr | Settlement emits durable events. Notification subscribers get them even if they were offline. |
| Polyglot | Java writes bids. Go settles. Node renders. None know about each other. |
| Smart Proxy | Browser-facing Node app reaches the Djinn through the proxy. No hardcoded ports. |

---

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                        Browser                              │
│   Auction Board — live bids, timers, results                │
│   Connected via SSE (Server-Sent Events)                    │
└───────────────────────┬─────────────────────────────────────┘
                        │ HTTP + SSE
┌───────────────────────▼─────────────────────────────────────┐
│              Auction Board (Node/TypeScript)                 │
│                                                             │
│   watch(auctions) → push to browser via SSE                 │
│   watch(bids)     → push to browser via SSE                 │
│   watch(sales)    → push to browser via SSE                 │
│   POST /bid       → write bid tuple to Space                │
│                                                             │
│   Discovers Djinn via Registry. No hardcoded addresses.     │
└───────────────────────┬─────────────────────────────────────┘
                        │ gRPC
┌───────────────────────▼─────────────────────────────────────┐
│                    The Djinn (Rust)                          │
│                                                             │
│   Space: auctions, bids, sales                              │
│   LeaseMgr: auction timers                                  │
│   EventMgr: settlement notifications                        │
│   TransactionMgr: atomic settlement                         │
│   Registry: service discovery                               │
└──────┬──────────────────────────────────┬───────────────────┘
       │ gRPC                             │ gRPC
┌──────▼──────────────┐    ┌──────────────▼──────────────────┐
│  Auction Service    │    │     Settlement Engine           │
│  (Spring Boot)      │    │     (Go)                        │
│                     │    │                                  │
│  POST /auction      │    │  watch(auctions, on="expire")   │
│    → write auction  │    │    → take(winning bid, txn)     │
│      tuple w/ TTL   │    │    → write(sale tuple, txn)     │
│                     │    │    → emit(settlement event)     │
│  Validates bids     │    │    → txn.commit()               │
│  Enforces rules     │    │                                  │
│  (min increment,    │    │  Registered as "Settlement"     │
│   reserve price)    │    │  in Registry                    │
│                     │    │                                  │
│  Registered as      │    │                                  │
│  "AuctionManager"   │    │                                  │
│  in Registry        │    │                                  │
└─────────────────────┘    └──────────────────────────────────┘
```

---

## Services

### 1. Auction Service — Spring Boot (Java)

**Role:** Auction lifecycle management and bid validation.

**Registry registration:**
```
interface: AuctionManager
attrs: { version: "1.0" }
```

**Responsibilities:**

Create auction — accepts item details, starting price, duration. Writes an auction tuple to the Space with a lease TTL equal to the auction duration. The lease IS the timer.

```java
space.write(Map.of(
    "type", "auction",
    "auction_id", uuid,
    "item", "Vintage Typewriter",
    "starting_price", "25.00",
    "reserve_price", "50.00",
    "current_bid", "0.00",
    "current_bidder", "",
    "status", "open"
), txn, Duration.ofSeconds(durationSeconds));  // lease = auction duration
```

Accept bid — validates bid against rules (auction still open, bid exceeds current + minimum increment, bidder is not already winning). Updates the auction tuple's current_bid and current_bidder. Writes a bid history tuple.

```java
// Validate
var auction = space.read(Map.of("type", "auction", "auction_id", auctionId));
if (newBid <= currentBid + MIN_INCREMENT) throw new BidTooLowException();

// Write bid record
space.write(Map.of(
    "type", "bid",
    "auction_id", auctionId,
    "bidder", bidderId,
    "amount", newBid.toString(),
    "timestamp", Instant.now().toString()
));
```

**Exposes:** REST API for the Node UI to call (POST /auction, POST /bid). The REST API is the only HTTP surface. All coordination goes through the Space.

**Spring Boot integration points:**
- `@PostConstruct` — register with Djinn Registry, start lease keepAlive
- `@PreDestroy` — cancel lease
- `SpaceClient` and `TransactionClient` injected as Spring beans
- `application.yml` for Djinn connection config

### 2. Settlement Engine — Go

**Role:** Watches for expired auctions and atomically settles them.

**Registry registration:**
```
interface: Settlement
attrs: { version: "1.0" }
```

**Responsibilities:**

Watch for auction expiry — uses `notify(template, on="expire")` on auction tuples. When an auction's lease expires, the auction is over.

Settle — inside a transaction:
1. Read the expired auction to get the winning bid details
2. Check if reserve price was met
3. Write a sale tuple to the Space (acts as the Ledger)
4. Emit a settlement event through EventMgr
5. Commit the transaction

```go
space.Notify(coordin8.Template{"type": "auction"}, func(event NotifyEvent) {
    if event.Type != EXPIRY {
        return
    }

    auction := event.Tuple

    txn, _ := txMgr.Begin(30 * time.Second)

    // Check reserve
    currentBid := parseFloat(auction["current_bid"])
    reservePrice := parseFloat(auction["reserve_price"])

    status := "sold"
    if currentBid < reservePrice {
        status = "reserve-not-met"
    }
    if auction["current_bidder"] == "" {
        status = "no-bids"
    }

    // Write sale record
    space.Write(map[string]string{
        "type":       "sale",
        "auction_id": auction["auction_id"],
        "item":       auction["item"],
        "status":     status,
        "winner":     auction["current_bidder"],
        "price":      auction["current_bid"],
        "settled_at": time.Now().Format(time.RFC3339),
    }, txn, 0)  // no TTL — sale record is permanent

    // Emit event for any subscribers
    eventMgr.Emit("settlement", "auction.settled", map[string]string{
        "auction_id": auction["auction_id"],
        "status":     status,
    })

    txn.Commit()
})
```

**Edge cases handled:**
- Reserve not met → sale tuple with status "reserve-not-met"
- No bids → sale tuple with status "no-bids"
- Settlement engine restarts → watches resume, no expired auctions missed (EventMgr durable delivery)

### 3. Auction Board — Node/TypeScript (SSR + SSE)

**Role:** Browser-facing UI. Renders live auction state. Pushes real-time updates.

**Registry registration:**
```
interface: AuctionBoard
attrs: { version: "1.0", role: "ui" }
```

**Responsibilities:**

Serve the auction board HTML page. Maintain SSE (Server-Sent Events) connections to connected browsers.

Watch the Space for:
- New auction tuples → show new auction card with countdown timer
- Bid tuples → update current bid display
- Sale tuples → show "SOLD" or "Reserve not met" overlay

```typescript
// Watch auctions appearing
space.notify({ type: "auction" }, (event) => {
    if (event.type === "MATCH") {
        broadcastSSE("auction:new", event.tuple);
    }
    if (event.type === "EXPIRY") {
        broadcastSSE("auction:ended", event.tuple);
    }
});

// Watch bids
space.notify({ type: "bid" }, (event) => {
    broadcastSSE("bid:placed", event.tuple);
});

// Watch sales
space.notify({ type: "sale" }, (event) => {
    broadcastSSE("sale:settled", event.tuple);
});
```

Proxy bid submissions from browser to auction service:
```typescript
app.post("/bid", async (req, res) => {
    // Write bid tuple directly to Space
    // (or call Auction Service via Registry discovery)
    const auctionMgr = await discovery.get(
        (addr) => new AuctionManagerClient(addr, creds),
        { interface: "AuctionManager" }
    );
    const result = await auctionMgr.placeBid(req.body);
    res.json(result);
});
```

**UI elements:**
- Auction cards with item name, current bid, countdown timer
- Bid input field per auction
- Live bid history feed
- "SOLD" / "No bids" / "Reserve not met" overlay when auction ends
- Connection status indicator (SSE connected/reconnecting)

**Tech stack:** Express + SSE for simplicity. No React, no build step. Plain HTML + vanilla JS on the client. The demo should be copy-paste runnable, not a frontend project.

---

## Tuple Schemas

### Auction tuple (leased — TTL = auction duration)
```json
{
    "type": "auction",
    "auction_id": "uuid",
    "item": "Vintage Typewriter",
    "description": "1952 Royal Quiet De Luxe",
    "image_url": "optional",
    "starting_price": "25.00",
    "reserve_price": "50.00",
    "current_bid": "0.00",
    "current_bidder": "",
    "status": "open"
}
```

### Bid tuple (permanent — history record)
```json
{
    "type": "bid",
    "auction_id": "uuid",
    "bidder": "bidder-name-or-id",
    "amount": "75.00",
    "timestamp": "2026-04-14T20:30:00Z"
}
```

### Sale tuple (permanent — ledger record)
```json
{
    "type": "sale",
    "auction_id": "uuid",
    "item": "Vintage Typewriter",
    "status": "sold | reserve-not-met | no-bids",
    "winner": "bidder-name-or-id",
    "price": "75.00",
    "settled_at": "2026-04-14T20:32:00Z"
}
```

---

## Auction Lifecycle (State Machine)

```
CREATE                 BID               BID              LEASE EXPIRES
  │                     │                 │                     │
  ▼                     ▼                 ▼                     ▼
┌──────┐  write()  ┌────────┐  write() ┌────────┐  expire  ┌──────────┐
│ NEW  │─────────►│  OPEN  │────────►│  OPEN  │────────►│ SETTLING │
│      │  auction  │ bid=0  │  bid    │ bid=75 │  lease  │          │
└──────┘  w/ TTL   └────────┘  tuple  └────────┘  dies   └────┬─────┘
                                                              │
                                                    take() + write()
                                                    in transaction
                                                              │
                                                              ▼
                                                        ┌──────────┐
                                                        │   SOLD   │
                                                        │  or      │
                                                        │  RESERVE │
                                                        │  NOT MET │
                                                        └──────────┘
```

The auction tuple is never explicitly closed. Its lease expires. That IS the close event. The settlement engine reacts to the expiry. This is "absence is a signal" as a user-visible feature.

---

## Demo Script (README walkthrough)

### Setup
```bash
cd examples/auction-house
docker compose up --build
# Djinn + Auction Service + Settlement Engine + Auction Board
# Browser opens to http://localhost:3000
```

### Step 1 — Create an auction
```bash
curl -X POST http://localhost:8080/auction \
  -H "Content-Type: application/json" \
  -d '{
    "item": "Vintage Typewriter",
    "description": "1952 Royal Quiet De Luxe",
    "starting_price": 25.00,
    "reserve_price": 50.00,
    "duration_seconds": 60
  }'
```
Browser shows: new auction card with 60-second countdown.

### Step 2 — Place bids
```bash
# Bidder 1
curl -X POST http://localhost:3000/bid \
  -d '{"auction_id": "...", "bidder": "Alice", "amount": 50.00}'

# Bidder 2
curl -X POST http://localhost:3000/bid \
  -d '{"auction_id": "...", "bidder": "Bob", "amount": 75.00}'
```
Browser shows: bids appear instantly. Current bid updates. Bid history scrolls.

### Step 3 — Watch the timer expire
Wait for the countdown to hit zero. No curl, no action needed.

Browser shows: "SOLD to Bob for $75.00" overlay appears on the auction card.

### Step 4 — Verify settlement
```bash
coordin8 space read --match type=sale
```
Shows the sale tuple with winner, price, timestamp.

### Step 5 — Kill the settlement engine, create another auction, restart
Proves durable EventMgr delivery — the settlement event was buffered while the engine was down, delivered on reconnect.

---

## Prerequisites (Java SDK Gaps)

The PRD.md in `.claude/plans/` identifies these gaps that must be closed before this demo:

| Gap | Required for | Priority |
|---|---|---|
| Java SpaceClient (hand-written wrapper) | Auction Service writing/reading tuples | **Blocker** |
| Java TransactionClient | Auction Service transactional bid acceptance | **Blocker** |
| Java LeaseClient.keepAlive | Auction Service auto-renewing its registration | **Blocker** |
| Java EventClient | Nice-to-have — settlement notifications | Deferrable |
| Node SpaceClient (hand-written wrapper) | Auction Board watching tuples | **Blocker** |

Go SDK is already at full parity — Settlement Engine is unblocked.

---

## Repo Structure

```
examples/
  auction-house/
    auction-service/              # Spring Boot (Java)
      src/
        main/java/.../
          AuctionApplication.java
          config/
            Coordin8Config.java   # Spring auto-configuration
          service/
            AuctionService.java   # Create auctions, validate bids
          controller/
            AuctionController.java  # REST endpoints
          model/
            Auction.java
            Bid.java
      build.gradle
      application.yml

    settlement-engine/            # Go
      main.go                     # Watch expired auctions, settle
      go.mod

    auction-board/                # Node/TypeScript
      src/
        server.ts                 # Express + SSE
        watchers.ts               # Space watches → SSE broadcast
      public/
        index.html                # Auction board UI
        app.js                    # SSE client, DOM updates
      package.json
      tsconfig.json

    docker-compose.yml            # Djinn + all three services
    Dockerfile.auction-service
    Dockerfile.settlement-engine
    Dockerfile.auction-board
    README.md                     # Demo walkthrough
    seed-auctions.sh              # Optional: pre-load sample auctions
```

---

## Docker Compose

```yaml
services:
  djinn:
    build:
      context: ../../
      dockerfile: Dockerfile.djinn
    ports:
      - "9001:9001"
      - "9002:9002"
      - "9003:9003"
      - "9004:9004"
      - "9005:9005"
      - "9006:9006"
      - "9100-9200:9100-9200"
    environment:
      RUST_LOG: "info"
      PROXY_BIND_HOST: "0.0.0.0"
    healthcheck:
      test: ["CMD-SHELL", "timeout 1 bash -c '</dev/tcp/localhost/9001'"]
      interval: 5s
      retries: 10

  auction-service:
    build: ./auction-service
    depends_on:
      djinn:
        condition: service_healthy
    environment:
      COORDIN8_REGISTRY_HOST: djinn
      COORDIN8_REGISTRY_PORT: 9002
      SPRING_PROFILES_ACTIVE: docker
    ports:
      - "8080:8080"

  settlement-engine:
    build: ./settlement-engine
    depends_on:
      djinn:
        condition: service_healthy
    environment:
      DJINN_HOST: djinn

  auction-board:
    build: ./auction-board
    depends_on:
      djinn:
        condition: service_healthy
      auction-service:
        condition: service_started
    environment:
      DJINN_HOST: djinn
      PORT: 3000
    ports:
      - "3000:3000"
```

---

## Success Criteria

1. `docker compose up --build` — everything starts, board is accessible at localhost:3000
2. Create auction via curl or board UI — card appears with countdown
3. Place bids from multiple "bidders" — updates appear instantly on board
4. Timer expires — settlement happens automatically, sale displayed
5. Kill settlement engine, create auction, let it expire, restart engine — settlement completes (durable events)
6. `coordin8 space read --match type=sale` — full audit trail
7. All three services discovered through Registry — no hardcoded service addresses
8. Total lines of coordination code across all three services < 200 (excluding boilerplate)

---

## What the Audience Takes Away

**If you're a Java developer:** "I can use Spring Boot with Coordin8 and my services coordinate with Go and Node services without any Spring Cloud dependency."

**If you're a platform engineer:** "One coordination plane replaces Eureka + Kafka + Redis + a saga framework."

**If you're evaluating Coordin8:** "The auction timer is a lease. The bid settlement is a transaction. The UI updates are reactive watches. This isn't plumbing — it's a coordination model."

**If you remember Jini:** "They brought it back. And it works with everything."
