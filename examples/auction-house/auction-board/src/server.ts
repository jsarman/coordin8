import express from "express";
import path from "path";
import { DjinnClient } from "@coordin8/sdk";

const PORT = parseInt(process.env.PORT || "3000", 10);
const DJINN_HOST = process.env.DJINN_HOST || "localhost";
const AUCTION_SERVICE = process.env.AUCTION_SERVICE || "http://localhost:8080";

const app = express();
app.use(express.json());
app.use(express.static(path.join(__dirname, "..", "public")));

// ── SSE connections ─────────────────────────────────────────────────────────

type SSEClient = { id: number; res: express.Response };
let clients: SSEClient[] = [];
let nextId = 0;

function broadcast(event: string, data: unknown) {
  const msg = `event: ${event}\ndata: ${JSON.stringify(data)}\n\n`;
  clients.forEach((c) => c.res.write(msg));
}

app.get("/events", (_req, res) => {
  res.writeHead(200, {
    "Content-Type": "text/event-stream",
    "Cache-Control": "no-cache",
    Connection: "keep-alive",
  });
  res.write("\n");
  const id = nextId++;
  clients.push({ id, res });
  _req.on("close", () => {
    clients = clients.filter((c) => c.id !== id);
  });
});

// ── Bid proxy ───────────────────────────────────────────────────────────────

app.post("/bid", async (req, res) => {
  try {
    const resp = await fetch(`${AUCTION_SERVICE}/bid`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(req.body),
    });
    const data = await resp.json();
    res.status(resp.status).json(data);
  } catch (err: any) {
    res.status(502).json({ error: err.message });
  }
});

// ── Create auction proxy ────────────────────────────────────────────────────

app.post("/auction", async (req, res) => {
  try {
    const resp = await fetch(`${AUCTION_SERVICE}/auction`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(req.body),
    });
    const data = await resp.json();
    res.status(resp.status).json(data);
  } catch (err: any) {
    res.status(502).json({ error: err.message });
  }
});

// ── Space watchers ──────────────────────────────────────────────────────────

async function watchLoop(
  djinn: DjinnClient,
  opts: { template: Record<string, string>; on: "appearance" | "expiration"; ttlSeconds: number },
  handler: (evt: import("@coordin8/sdk").SpaceEvent) => void
) {
  for await (const evt of djinn.space().watch(opts)) {
    handler(evt);
  }
}

function startWatchers() {
  const djinn = DjinnClient.connect(DJINN_HOST);

  console.log("  watching auctions (appearance + expiry), bids, sales...");

  watchLoop(djinn, { template: { type: "auction" }, on: "appearance", ttlSeconds: 600 }, (evt) => {
    if (evt.tuple) broadcast("auction:new", { ...evt.tuple.attrs, tuple_id: evt.tuple.tupleId });
  });

  watchLoop(djinn, { template: { type: "auction" }, on: "expiration", ttlSeconds: 600 }, (evt) => {
    if (evt.tuple) broadcast("auction:ended", { ...evt.tuple.attrs, tuple_id: evt.tuple.tupleId });
  });

  watchLoop(djinn, { template: { type: "bid" }, on: "appearance", ttlSeconds: 600 }, (evt) => {
    if (evt.tuple) broadcast("bid:placed", evt.tuple.attrs);
  });

  watchLoop(djinn, { template: { type: "sale" }, on: "appearance", ttlSeconds: 600 }, (evt) => {
    if (evt.tuple) broadcast("sale:settled", evt.tuple.attrs);
  });
}

// ── Boot ────────────────────────────────────────────────────────────────────

app.listen(PORT, () => {
  console.log(`Auction Board listening on http://localhost:${PORT}`);
  startWatchers();
});
