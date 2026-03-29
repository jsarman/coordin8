package io.coordin8;

import io.grpc.ManagedChannel;
import io.grpc.ManagedChannelBuilder;

import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import java.util.TreeMap;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.TimeUnit;
import java.util.function.Function;
import java.util.stream.Collectors;

/**
 * Jini-inspired service discovery manager.
 *
 * <p>Caches proxies by template. Repeated calls with the same template return
 * a cached stub without any additional Djinn round-trips.
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

    private record CachedEntry(String proxyId, ManagedChannel channel) {}

    private final DjinnClient djinn;
    private final ConcurrentHashMap<String, CachedEntry> cache = new ConcurrentHashMap<>();

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
     * Subsequent calls with the same template return a cached stub.
     *
     * @param factory  method reference or lambda — e.g. {@code GreeterServiceGrpc::newBlockingStub}
     * @param template attribute map to match against the Registry
     */
    public <T> T get(Function<ManagedChannel, T> factory, Map<String, String> template) {
        String key = templateKey(template);
        CachedEntry entry = cache.computeIfAbsent(key, k -> openEntry(template));
        return factory.apply(entry.channel());
    }

    private CachedEntry openEntry(Map<String, String> template) {
        ProxyClient.ProxyHandleRecord handle = djinn.proxy().open(template);
        ManagedChannel channel = ManagedChannelBuilder
                .forAddress("localhost", handle.localPort())
                .usePlaintext()
                .build();
        return new CachedEntry(handle.proxyId(), channel);
    }

    /**
     * Release all cached proxies and channels.
     */
    @Override
    public void close() throws InterruptedException {
        List<CachedEntry> entries = new ArrayList<>(cache.values());
        cache.clear();
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
