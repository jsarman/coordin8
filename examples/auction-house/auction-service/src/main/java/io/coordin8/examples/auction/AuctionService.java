package io.coordin8.examples.auction;

import com.sun.net.httpserver.HttpExchange;
import com.sun.net.httpserver.HttpServer;
import io.coordin8.DjinnClient;
import io.coordin8.SpaceClient;

import java.io.Closeable;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.net.InetSocketAddress;
import java.nio.charset.StandardCharsets;
import java.time.Duration;
import java.util.Map;
import java.util.UUID;

/**
 * Auction Service — creates auctions and validates bids.
 * <p>
 * REST endpoints:
 * <ul>
 *   <li>POST /auction — create a new auction (writes a leased tuple)</li>
 *   <li>POST /bid — place a bid (validates then writes a bid tuple)</li>
 * </ul>
 * <p>
 * All coordination goes through the Space. The REST API is just the browser-facing surface.
 */
public class AuctionService {

    private static final double MIN_INCREMENT = 1.00;

    private final DjinnClient djinn;
    private final SpaceClient space;

    public AuctionService(DjinnClient djinn) {
        this.djinn = djinn;
        this.space = djinn.space();
    }

    // ── Create Auction ──────────────────────────────────────────────────────

    public Map<String, Object> createAuction(String item, double startingPrice,
                                              double reservePrice, int durationSeconds) {
        String auctionId = UUID.randomUUID().toString();
        long endEpoch = System.currentTimeMillis() / 1000 + durationSeconds;

        var tuple = space.write(
                Map.of(
                        "type", "auction",
                        "auction_id", auctionId,
                        "item", item,
                        "starting_price", String.format("%.2f", startingPrice),
                        "reserve_price", String.format("%.2f", reservePrice),
                        "current_bid", "0.00",
                        "current_bidder", "",
                        "status", "open",
                        "duration_seconds", String.valueOf(durationSeconds),
                        "end_epoch", String.valueOf(endEpoch)
                ),
                null, durationSeconds, "auction-service", null, null);

        System.out.printf("  + Created auction: %s (%s, %ds)%n", item, auctionId, durationSeconds);
        return Map.of("auction_id", auctionId, "tuple_id", tuple.tupleId(), "ttl", durationSeconds);
    }

    // ── Place Bid ───────────────────────────────────────────────────────────

    public Map<String, Object> placeBid(String auctionId, String bidder, double amount) {
        // Atomic take — validates and removes in one step (no read/take TOCTOU race)
        var taken = space.take(Map.of("type", "auction", "auction_id", auctionId));
        if (taken.isEmpty()) {
            throw new IllegalStateException("Auction not found or already ended");
        }

        var attrs = taken.get().attrs();
        double currentBid = Double.parseDouble(attrs.getOrDefault("current_bid", "0"));
        String currentBidder = attrs.getOrDefault("current_bidder", "");

        // Validate — if invalid, re-write the original tuple before throwing
        try {
            if (amount <= currentBid + MIN_INCREMENT && currentBid > 0) {
                throw new IllegalArgumentException(
                        String.format("Bid must exceed current bid ($%.2f) by at least $%.2f", currentBid, MIN_INCREMENT));
            }
            double startingPrice = Double.parseDouble(attrs.getOrDefault("starting_price", "0"));
            if (amount < startingPrice) {
                throw new IllegalArgumentException(
                        String.format("Bid must be at least the starting price ($%.2f)", startingPrice));
            }
            if (bidder.equals(currentBidder)) {
                throw new IllegalArgumentException("You are already the highest bidder");
            }
        } catch (IllegalArgumentException e) {
            // Put the auction back untouched
            long remainingTtl = computeRemainingTtl(attrs);
            space.write(attrs, null, remainingTtl, "auction-service", null, null);
            throw e;
        }

        // Compute remaining TTL from stored end_epoch so bids don't reset the clock
        long remainingTtl = computeRemainingTtl(attrs);

        // Write bid record (permanent — outlives the auction for audit trail)
        space.write(Map.of(
                "type", "bid",
                "auction_id", auctionId,
                "bidder", bidder,
                "amount", String.format("%.2f", amount),
                "timestamp", java.time.Instant.now().toString()
        ), 0);

        // Re-write auction with updated bid, preserving remaining lease time
        var updatedAttrs = new java.util.HashMap<>(attrs);
        updatedAttrs.put("current_bid", String.format("%.2f", amount));
        updatedAttrs.put("current_bidder", bidder);
        space.write(updatedAttrs, null, remainingTtl, "auction-service", null, null);

        System.out.printf("  $ Bid: $%.2f by %s on %s%n", amount, bidder, auctionId.substring(0, 8));
        return Map.of("auction_id", auctionId, "bidder", bidder, "amount", amount, "status", "accepted");
    }

    private static long computeRemainingTtl(Map<String, String> attrs) {
        String endEpochStr = attrs.get("end_epoch");
        if (endEpochStr != null) {
            long remaining = Long.parseLong(endEpochStr) - (System.currentTimeMillis() / 1000);
            return Math.max(remaining, 1);
        }
        return Long.parseLong(attrs.getOrDefault("duration_seconds", "30"));
    }

    // ── HTTP Server ─────────────────────────────────────────────────────────

    public static void main(String[] args) throws Exception {
        String djinnHost = System.getenv().getOrDefault("DJINN_HOST", "localhost");
        int port = Integer.parseInt(System.getenv().getOrDefault("PORT", "8080"));

        System.out.println("Auction Service starting...");
        System.out.printf("  djinn: %s%n", djinnHost);

        DjinnClient djinn = DjinnClient.connect(djinnHost);
        AuctionService service = new AuctionService(djinn);

        // Register with Djinn
        var reg = djinn.registry().register("AuctionManager",
                Map.of("version", "1.0"), 30, null);
        Closeable keepAlive = djinn.leases().keepAlive(reg.leaseId(), 30);
        System.out.printf("  registered: AuctionManager (lease=%s)%n", reg.leaseId());

        // Start HTTP server
        HttpServer server = HttpServer.create(new InetSocketAddress(port), 0);

        server.createContext("/auction", exchange -> {
            addCors(exchange);
            if ("OPTIONS".equals(exchange.getRequestMethod())) { sendJson(exchange, 200, "{}"); return; }
            if (!"POST".equals(exchange.getRequestMethod())) { sendJson(exchange, 405, "{\"error\":\"POST only\"}"); return; }
            try {
                var body = parseJson(exchange.getRequestBody());
                var result = service.createAuction(
                        (String) body.get("item"),
                        toDouble(body.get("starting_price")),
                        toDouble(body.get("reserve_price")),
                        toInt(body.get("duration_seconds")));
                sendJson(exchange, 200, toJson(result));
            } catch (Exception e) {
                sendJson(exchange, 400, "{\"error\":\"" + e.getMessage().replace("\"", "'") + "\"}");
            }
        });

        server.createContext("/bid", exchange -> {
            addCors(exchange);
            if ("OPTIONS".equals(exchange.getRequestMethod())) { sendJson(exchange, 200, "{}"); return; }
            if (!"POST".equals(exchange.getRequestMethod())) { sendJson(exchange, 405, "{\"error\":\"POST only\"}"); return; }
            try {
                var body = parseJson(exchange.getRequestBody());
                var result = service.placeBid(
                        (String) body.get("auction_id"),
                        (String) body.get("bidder"),
                        toDouble(body.get("amount")));
                sendJson(exchange, 200, toJson(result));
            } catch (Exception e) {
                sendJson(exchange, 400, "{\"error\":\"" + e.getMessage().replace("\"", "'") + "\"}");
            }
        });

        server.start();
        System.out.printf("Auction Service ready on :%d%n", port);

        Runtime.getRuntime().addShutdownHook(new Thread(() -> {
            System.out.println("\nShutting down...");
            try { keepAlive.close(); } catch (Exception ignored) {}
            try { djinn.leases().cancel(reg.leaseId()); } catch (Exception ignored) {}
            server.stop(0);
            try { djinn.close(); } catch (Exception ignored) {}
        }));
    }

    // ── Minimal JSON helpers (no library dependency) ────────────────────────

    @SuppressWarnings("unchecked")
    private static Map<String, Object> parseJson(InputStream is) throws IOException {
        String raw = new String(is.readAllBytes(), StandardCharsets.UTF_8).trim();
        // Simple JSON parser for flat objects — handles commas inside quoted strings
        Map<String, Object> map = new java.util.HashMap<>();
        if (raw.startsWith("{")) raw = raw.substring(1);
        if (raw.endsWith("}")) raw = raw.substring(0, raw.length() - 1);

        // Split on commas that aren't inside quotes
        java.util.List<String> pairs = new java.util.ArrayList<>();
        StringBuilder current = new StringBuilder();
        boolean inQuotes = false;
        for (int i = 0; i < raw.length(); i++) {
            char c = raw.charAt(i);
            if (c == '"' && (i == 0 || raw.charAt(i - 1) != '\\')) {
                inQuotes = !inQuotes;
            }
            if (c == ',' && !inQuotes) {
                pairs.add(current.toString());
                current.setLength(0);
            } else {
                current.append(c);
            }
        }
        if (current.length() > 0) pairs.add(current.toString());

        for (String pair : pairs) {
            String[] kv = pair.split(":", 2);
            if (kv.length == 2) {
                String key = kv[0].trim().replace("\"", "");
                String val = kv[1].trim();
                if (val.startsWith("\"") && val.endsWith("\"")) {
                    map.put(key, val.substring(1, val.length() - 1));
                } else {
                    try { map.put(key, Double.parseDouble(val)); }
                    catch (NumberFormatException e) { map.put(key, val); }
                }
            }
        }
        return map;
    }

    private static String toJson(Map<String, Object> map) {
        StringBuilder sb = new StringBuilder("{");
        boolean first = true;
        for (var entry : map.entrySet()) {
            if (!first) sb.append(",");
            sb.append("\"").append(entry.getKey()).append("\":");
            if (entry.getValue() instanceof Number) {
                sb.append(entry.getValue());
            } else {
                sb.append("\"").append(entry.getValue()).append("\"");
            }
            first = false;
        }
        return sb.append("}").toString();
    }

    private static void addCors(HttpExchange exchange) {
        exchange.getResponseHeaders().add("Access-Control-Allow-Origin", "*");
        exchange.getResponseHeaders().add("Access-Control-Allow-Methods", "POST, OPTIONS");
        exchange.getResponseHeaders().add("Access-Control-Allow-Headers", "Content-Type");
    }

    private static void sendJson(HttpExchange exchange, int status, String json) throws IOException {
        byte[] bytes = json.getBytes(StandardCharsets.UTF_8);
        exchange.getResponseHeaders().add("Content-Type", "application/json");
        exchange.sendResponseHeaders(status, bytes.length);
        try (OutputStream os = exchange.getResponseBody()) {
            os.write(bytes);
        }
    }

    private static double toDouble(Object v) {
        if (v instanceof Number n) return n.doubleValue();
        return Double.parseDouble(String.valueOf(v));
    }

    private static int toInt(Object v) {
        if (v instanceof Number n) return n.intValue();
        return Integer.parseInt(String.valueOf(v));
    }
}
