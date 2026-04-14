// Auction House — browser-side SSE client + DOM updates
// No build step, no framework. Just vanilla JS.

const auctions = {};  // auction_id → { element, endTime, timer, ... }

// ── SSE connection ──────────────────────────────────────────────────────────

const es = new EventSource("/events");
const dot = document.getElementById("dot");
const statusText = document.getElementById("status-text");

es.onopen = () => {
  dot.className = "dot connected";
  statusText.textContent = "Connected";
};
es.onerror = () => {
  dot.className = "dot disconnected";
  statusText.textContent = "Reconnecting...";
};

es.addEventListener("auction:new", (e) => {
  const data = JSON.parse(e.data);
  addAuction(data);
  logEvent("bid", `New auction: ${data.item} (starting $${data.starting_price})`);
});

es.addEventListener("auction:ended", (e) => {
  const data = JSON.parse(e.data);
  const a = auctions[data.auction_id];
  if (a) {
    clearInterval(a.timer);
    a.timerEl.textContent = "0:00";
    a.timerEl.classList.add("urgent");
  }
  logEvent("expire", `Auction ended: ${data.item}`);
});

es.addEventListener("bid:placed", (e) => {
  const data = JSON.parse(e.data);
  const a = auctions[data.auction_id];
  if (a) {
    a.bidEl.textContent = `$${parseFloat(data.amount).toFixed(2)}`;
    a.bidderEl.textContent = data.bidder || "—";
  }
  logEvent("bid", `Bid: $${data.amount} by ${data.bidder} on ${data.auction_id.slice(0, 8)}...`);
});

es.addEventListener("sale:settled", (e) => {
  const data = JSON.parse(e.data);
  const a = auctions[data.auction_id];
  if (a) {
    a.el.classList.add("ended");
    const overlay = document.createElement("div");
    overlay.className = "overlay " + (data.status === "sold" ? "sold" : "no-sale");
    if (data.status === "sold") {
      overlay.textContent = `Sold to ${data.winner} for $${parseFloat(data.price).toFixed(2)}`;
    } else if (data.status === "reserve-not-met") {
      overlay.textContent = "Reserve Not Met";
    } else {
      overlay.textContent = "No Bids";
    }
    a.el.appendChild(overlay);
    // Hide the bid form
    const form = a.el.querySelector(".bid-form");
    if (form) form.style.display = "none";
  }
  logEvent("sale", `Settled: ${data.item} — ${data.status}${data.status === "sold" ? ` ($${data.price} to ${data.winner})` : ""}`);
});

// ── Auction card rendering ──────────────────────────────────────────────────

function addAuction(data) {
  if (auctions[data.auction_id]) return; // dedupe

  const grid = document.getElementById("auctions");
  const el = document.createElement("div");
  el.className = "card";
  el.innerHTML = `
    <div class="item">${esc(data.item)}</div>
    <div class="auction-id">${esc(data.auction_id)}</div>
    <div class="timer" id="timer-${data.auction_id}">--:--</div>
    <div class="bid-info">
      <div><div class="label">Current Bid</div><div class="value" id="bid-${data.auction_id}">$${parseFloat(data.current_bid || data.starting_price).toFixed(2)}</div></div>
      <div><div class="label">Bidder</div><div class="value" id="bidder-${data.auction_id}">${esc(data.current_bidder || "—")}</div></div>
      <div><div class="label">Reserve</div><div class="value">$${parseFloat(data.reserve_price).toFixed(2)}</div></div>
    </div>
    <div class="bid-form">
      <input type="number" id="amount-${data.auction_id}" placeholder="Your bid $" step="0.01">
      <input type="text" id="name-${data.auction_id}" placeholder="Your name" style="width:100px">
      <button onclick="placeBid('${data.auction_id}')">Bid</button>
    </div>
  `;
  grid.prepend(el);

  const timerEl = document.getElementById(`timer-${data.auction_id}`);
  const bidEl = document.getElementById(`bid-${data.auction_id}`);
  const bidderEl = document.getElementById(`bidder-${data.auction_id}`);

  // Estimate end time from tuple lease (approx — the board doesn't know exact expiry)
  const durationMs = (parseInt(data.duration_seconds) || 30) * 1000;
  const endTime = Date.now() + durationMs;

  const timer = setInterval(() => {
    const remaining = Math.max(0, endTime - Date.now());
    const secs = Math.ceil(remaining / 1000);
    const m = Math.floor(secs / 60);
    const s = secs % 60;
    timerEl.textContent = `${m}:${s.toString().padStart(2, "0")}`;
    if (secs <= 10) timerEl.classList.add("urgent");
    if (secs <= 0) clearInterval(timer);
  }, 250);

  auctions[data.auction_id] = { el, timerEl, bidEl, bidderEl, timer, endTime };
}

// ── Actions ─────────────────────────────────────────────────────────────────

async function placeBid(auctionId) {
  const amountEl = document.getElementById(`amount-${auctionId}`);
  const nameEl = document.getElementById(`name-${auctionId}`);
  const amount = parseFloat(amountEl.value);
  const bidder = nameEl.value.trim() || "Anonymous";
  if (!amount || amount <= 0) return;

  try {
    const resp = await fetch("/bid", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ auction_id: auctionId, bidder, amount }),
    });
    const data = await resp.json();
    if (!resp.ok) {
      logEvent("expire", `Bid rejected: ${data.error || resp.statusText}`);
    }
    amountEl.value = "";
  } catch (err) {
    logEvent("expire", `Bid failed: ${err.message}`);
  }
}

async function createAuction() {
  const item = document.getElementById("item").value.trim();
  const starting = parseFloat(document.getElementById("starting").value);
  const reserve = parseFloat(document.getElementById("reserve").value);
  const duration = parseInt(document.getElementById("duration").value);
  if (!item) return;

  try {
    const resp = await fetch("/auction", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        item,
        starting_price: starting,
        reserve_price: reserve,
        duration_seconds: duration,
      }),
    });
    if (!resp.ok) {
      const data = await resp.json();
      logEvent("expire", `Create failed: ${data.error || resp.statusText}`);
    }
  } catch (err) {
    logEvent("expire", `Create failed: ${err.message}`);
  }
}

// ── Helpers ─────────────────────────────────────────────────────────────────

function logEvent(cls, text) {
  const log = document.getElementById("log");
  const div = document.createElement("div");
  div.className = cls;
  div.textContent = `[${new Date().toLocaleTimeString()}] ${text}`;
  log.prepend(div);
  // Keep log bounded
  while (log.children.length > 100) log.removeChild(log.lastChild);
}

function esc(s) {
  const el = document.createElement("span");
  el.textContent = s || "";
  return el.innerHTML;
}
