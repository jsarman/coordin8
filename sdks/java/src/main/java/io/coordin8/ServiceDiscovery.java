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
 * <p>Caches proxies by template. Repeated calls with the same template return
 * a cached stub without any additional Djinn round-trips. Watches the Registry
 * for changes and invalidates/refreshes cached connections automatically.
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

    private record CachedEntry(String proxyId, ManagedChannel channel, AtomicBoolean stale) {
        CachedEntry(String proxyId, ManagedChannel channel) {
            this(proxyId, channel, new AtomicBoolean(false));
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
     * the entry has been invalidated by a Registry Watch event.
     *
     * @param factory  method reference or lambda — e.g. {@code GreeterServiceGrpc::newBlockingStub}
     * @param template attribute map to match against the Registry
     */
    public <T> T get(Function<ManagedChannel, T> factory, Map<String, String> template) {
        String key = templateKey(template);
        CachedEntry entry = cache.get(key);
        if (entry != null && !entry.stale().get()) {
            return factory.apply(entry.channel());
        }
        entry = refresh(key, template);
        return factory.apply(entry.channel());
    }

    private synchronized CachedEntry refresh(String key, Map<String, String> template) {
        // Double-check under lock
        CachedEntry existing = cache.get(key);
        if (existing != null && !existing.stale().get()) {
            return existing;
        }

        // Close old entry if stale
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
        return new CachedEntry(handle.proxyId(), channel);
    }

    private void startWatch(String key, Map<String, String> template) {
        djinn.registry().watch(template,
                evt -> {
                    if (closed) return;
                    switch (evt.type()) {
                        case "expired" -> {
                            CachedEntry entry = cache.get(key);
                            if (entry != null) {
                                entry.stale().set(true);
                            }
                        }
                        case "registered" -> {
                            CachedEntry entry = cache.get(key);
                            if (entry != null && entry.stale().get()) {
                                refresh(key, template);
                            }
                        }
                        case "modified" -> {
                            CachedEntry entry = cache.get(key);
                            if (entry != null) {
                                entry.stale().set(true);
                            }
                            refresh(key, template);
                        }
                    }
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
        djinn.proxy().release(entry.proxyId());
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
            djinn.proxy().release(entry.proxyId());
        }
    }

    private static String templateKey(Map<String, String> template) {
        return new TreeMap<>(template).entrySet().stream()
                .map(e -> e.getKey() + "=" + e.getValue())
                .collect(Collectors.joining(","));
    }
}
