package io.coordin8;

import io.grpc.ManagedChannel;
import io.grpc.ManagedChannelBuilder;

import java.util.Map;
import java.util.Optional;
import java.util.concurrent.TimeUnit;

/**
 * Entry point for all Djinn interactions.
 *
 * <pre>{@code
 * try (DjinnClient djinn = DjinnClient.connect("localhost:9002")) {
 *     LeaseClient    leases   = djinn.registryLeases();
 *     RegistryClient registry = djinn.registry();
 *     ProxyClient    proxy    = djinn.proxy();
 * }
 * }</pre>
 *
 * <p>There is no single "LeaseMgr" to connect to — leasing is distributed.
 * Registry, Space, and EventMgr each grant their own leases and mount
 * {@code LeaseService} on their own connection; renew/cancel a lease by
 * dialing whichever one granted it (its address is carried on the lease
 * itself, in {@code grantorHost}/{@code grantorPort}) via
 * {@link LeaseClient#dial(String)}. See
 * {@code .claude/plans/distributed-leasing/PRD.md}.
 */
public class DjinnClient implements AutoCloseable {

    private final ManagedChannel registryChannel;
    private final ManagedChannel proxyChannel;
    private final ManagedChannel spaceChannel;
    private final ManagedChannel eventChannel;

    private DjinnClient(ManagedChannel registryChannel,
                        ManagedChannel proxyChannel,
                        ManagedChannel spaceChannel,
                        ManagedChannel eventChannel) {
        this.registryChannel = registryChannel;
        this.proxyChannel    = proxyChannel;
        this.spaceChannel    = spaceChannel;
        this.eventChannel    = eventChannel;
    }

    /**
     * Dial Registry directly at {@code registryAddr} — the one address a
     * caller needs to know in advance — then look up Proxy, Space, and
     * EventMgr through it, the same way any application service is
     * discovered via {@link ServiceDiscovery}. Works identically against a
     * bundled monolith or a fully split, multi-host deployment: Registry
     * just returns whatever address each service actually registered.
     *
     * @param registryAddr Registry's address, {@code "host:port"}
     */
    public static DjinnClient connect(String registryAddr) {
        return connect(registryAddr, null, null, null);
    }

    /**
     * Same as {@link #connect(String)}, but lets a caller pin Proxy/Space/
     * EventMgr's address explicitly instead of looking it up through
     * Registry — an escape hatch mirroring the Go SDK's
     * {@code WithProxyAddr}/{@code WithSpaceAddr}/{@code WithEventAddr}.
     * Pass {@code null} for any address that should be resolved via
     * {@code Registry.Lookup} instead.
     *
     * @param registryAddr Registry's address, {@code "host:port"}
     * @param proxyAddr    pinned Proxy address, or {@code null} to look up
     * @param spaceAddr    pinned Space address, or {@code null} to look up
     * @param eventAddr    pinned EventMgr address, or {@code null} to look up
     */
    public static DjinnClient connect(String registryAddr, String proxyAddr,
                                      String spaceAddr, String eventAddr) {
        ManagedChannel registryChannel = channelFor(registryAddr);
        ManagedChannel proxyChannel = null;
        ManagedChannel spaceChannel = null;
        ManagedChannel eventChannel = null;
        try {
            RegistryClient registryClient = new RegistryClient(registryChannel);

            proxyChannel = channelFor(resolve(registryClient, proxyAddr, "Proxy"));
            spaceChannel = channelFor(resolve(registryClient, spaceAddr, "Space"));
            eventChannel = channelFor(resolve(registryClient, eventAddr, "EventMgr"));

            return new DjinnClient(registryChannel, proxyChannel, spaceChannel, eventChannel);
        } catch (RuntimeException e) {
            registryChannel.shutdown();
            if (proxyChannel != null) proxyChannel.shutdown();
            if (spaceChannel != null) spaceChannel.shutdown();
            if (eventChannel != null) eventChannel.shutdown();
            throw e;
        }
    }

    private static String resolve(RegistryClient registryClient, String pinned, String interfaceName) {
        if (pinned != null) {
            return pinned;
        }
        Optional<RegistryClient.CapabilityRecord> cap =
                registryClient.lookup(Map.of("interface", interfaceName));
        RegistryClient.CapabilityRecord record = cap.orElseThrow(() ->
                new IllegalStateException("look up " + interfaceName + ": not found in registry"));
        RegistryClient.TransportDescriptor transport = record.transport();
        if (transport == null) {
            throw new IllegalStateException("look up " + interfaceName + ": no transport in registry entry");
        }
        String host = transport.config().get("host");
        String port = transport.config().get("port");
        if (host == null || host.isEmpty() || port == null || port.isEmpty()) {
            throw new IllegalStateException("look up " + interfaceName + ": missing host/port in transport config");
        }
        return host + ":" + port;
    }

    private static ManagedChannel channelFor(String addr) {
        int idx = addr.lastIndexOf(':');
        if (idx < 0) {
            throw new IllegalArgumentException("address must be host:port, got: " + addr);
        }
        String host = addr.substring(0, idx);
        int port = Integer.parseInt(addr.substring(idx + 1));
        return ManagedChannelBuilder.forAddress(host, port).usePlaintext().build();
    }

    /**
     * Returns a {@link LeaseClient} for renewing/cancelling leases that
     * Registry itself granted — every self-registration via
     * {@code registry().register(...)} returns one. {@code LeaseService} is
     * mounted on Registry's own connection (no separate dial needed),
     * matching how Registry embeds its own {@code LeaseManager}.
     */
    public LeaseClient registryLeases() { return LeaseClient.wrap(registryChannel); }

    public RegistryClient registry() { return new RegistryClient(registryChannel); }
    public ProxyClient    proxy()    { return new ProxyClient(proxyChannel); }
    public SpaceClient    space()    { return new SpaceClient(spaceChannel); }
    public EventClient    events()   { return new EventClient(eventChannel); }

    @Override
    public void close() throws InterruptedException {
        registryChannel.shutdown().awaitTermination(5, TimeUnit.SECONDS);
        proxyChannel.shutdown().awaitTermination(5, TimeUnit.SECONDS);
        spaceChannel.shutdown().awaitTermination(5, TimeUnit.SECONDS);
        eventChannel.shutdown().awaitTermination(5, TimeUnit.SECONDS);
    }
}
