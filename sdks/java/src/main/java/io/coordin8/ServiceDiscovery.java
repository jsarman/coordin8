package io.coordin8;

import io.grpc.ManagedChannel;
import io.grpc.ManagedChannelBuilder;

import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import java.util.TreeMap;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.function.Function;
import java.util.stream.Collectors;

/**
 * Jini-inspired service discovery manager.
 *
 * <p>Keeps one leased proxy per template. Repeated calls with the same
 * template return a cached stub without any additional Djinn round-trips.
 * Proxy re-resolves the upstream on every new TCP connection, so a held
 * channel survives the service expiring, restarting or moving: while the
 * service is absent, RPCs fail at connect time and succeed again once it
 * re-registers, with no client action. The cache entry is replaced only if
 * the proxy's own lease is lost.
 *
 * <pre>{@code
 * var discovery = ServiceDiscovery.watch(djinn);
 *
 * var greeter = discovery.get(
 *     GreeterServiceGrpc::newBlockingStub,
 *     Map.of("interface", "Greeter")
 * );
 * greeter.hello(...);
 *
 * discovery.close();
 * }</pre>
 */
public class ServiceDiscovery implements AutoCloseable {

    private record CachedEntry(String proxyId, ManagedChannel channel,
                               ProxyClient.ProxyHandleRecord handle) {
        CachedEntry(ProxyClient.ProxyHandleRecord handle, ManagedChannel channel) {
            this(handle.proxyId(), channel, handle);
        }

        /** Only stale when the Djinn reclaimed the proxy's own lease. */
        boolean isStale() {
            return handle.lost().get();
        }
    }

    private final DjinnClient djinn;
    private final ConcurrentHashMap<String, CachedEntry> cache = new ConcurrentHashMap<>();
    private final ConcurrentHashMap<String, AtomicBoolean> watching = new ConcurrentHashMap<>();
    private volatile boolean closed = false;

    private ServiceDiscovery(DjinnClient djinn) {
        this.djinn = djinn;
    }

    /**
     * Create a ServiceDiscovery manager backed by the given DjinnClient.
     */
    public static ServiceDiscovery watch(DjinnClient djinn) {
        return new ServiceDiscovery(djinn);
    }

    /**
     * Return a ready stub for the first capability matching template.
     * Subsequent calls with the same template return a cached stub unless
     * the proxy's own lease was lost (the Djinn reclaimed it), in which case a
     * new proxy is opened and the dead one released.
     *
     * @param factory  method reference or lambda — e.g. {@code GreeterServiceGrpc::newBlockingStub}
     * @param template attribute map to match against the Registry
     */
    public <T> T get(Function<ManagedChannel, T> factory, Map<String, String> template) {
        String key = templateKey(template);
        CachedEntry entry = cache.get(key);
        if (entry != null && !entry.isStale()) {
            return factory.apply(entry.channel());
        }
        entry = refresh(key, template);
        return factory.apply(entry.channel());
    }

    private synchronized CachedEntry refresh(String key, Map<String, String> template) {
        // Double-check under lock
        CachedEntry existing = cache.get(key);
        if (existing != null && !existing.isStale()) {
            return existing;
        }

        // The entry is only ever replaced because its proxy lease was lost, so
        // the proxy (and any channel a caller holds on it) is already dead.
        if (existing != null) {
            closeEntry(existing);
        }

        CachedEntry entry = openEntry(template);
        cache.put(key, entry);

        // Start watching if not already
        if (!watching.containsKey(key)) {
            watching.put(key, new AtomicBoolean(true));
            startWatch(key, template);
        }

        return entry;
    }

    private CachedEntry openEntry(Map<String, String> template) {
        ProxyClient.ProxyHandleRecord handle = djinn.proxy().open(template);
        ManagedChannel channel = ManagedChannelBuilder
                .forAddress("localhost", handle.localPort())
                .usePlaintext()
                .build();
        return new CachedEntry(handle, channel);
    }

    private void startWatch(String key, Map<String, String> template) {
        djinn.registry().watch(template,
                evt -> {
                    if (closed) return;
                    // "expired" / "registered" / "modified": nothing to do. Proxy
                    // re-resolves the upstream on every new TCP connection, so the
                    // existing proxy stays valid while the service restarts or
                    // moves. Only a lost proxy lease replaces an entry.
                },
                err -> {
                    if (closed) return;
                    // Reconnect after a brief pause
                    watching.remove(key);
                    Thread.ofVirtual().start(() -> {
                        try {
                            Thread.sleep(2000);
                        } catch (InterruptedException ignored) {
                            Thread.currentThread().interrupt();
                            return;
                        }
                        if (!closed && cache.containsKey(key)) {
                            watching.put(key, new AtomicBoolean(true));
                            startWatch(key, template);
                        }
                    });
                }
        );
    }

    private void closeEntry(CachedEntry entry) {
        try {
            entry.channel().shutdown().awaitTermination(2, TimeUnit.SECONDS);
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
        }
        entry.handle().close();
    }

    /**
     * Release all cached proxies and channels.
     */
    @Override
    public void close() throws InterruptedException {
        closed = true;
        List<CachedEntry> entries = new ArrayList<>(cache.values());
        cache.clear();
        watching.clear();
        for (CachedEntry entry : entries) {
            entry.channel().shutdown().awaitTermination(5, TimeUnit.SECONDS);
            entry.handle().close();
        }
    }

    private static String templateKey(Map<String, String> template) {
        return new TreeMap<>(template).entrySet().stream()
                .map(e -> e.getKey() + "=" + e.getValue())
                .collect(Collectors.joining(","));
    }
}
