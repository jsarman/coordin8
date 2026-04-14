package io.coordin8;

import io.grpc.ManagedChannel;
import io.grpc.ManagedChannelBuilder;

import java.util.concurrent.TimeUnit;

/**
 * Entry point for all Djinn interactions.
 *
 * <pre>{@code
 * try (DjinnClient djinn = DjinnClient.connect("localhost")) {
 *     LeaseClient    leases   = djinn.leases();
 *     RegistryClient registry = djinn.registry();
 *     ProxyClient    proxy    = djinn.proxy();
 * }
 * }</pre>
 */
public class DjinnClient implements AutoCloseable {

    private final ManagedChannel leaseChannel;
    private final ManagedChannel registryChannel;
    private final ManagedChannel proxyChannel;
    private final ManagedChannel spaceChannel;

    private DjinnClient(ManagedChannel leaseChannel,
                        ManagedChannel registryChannel,
                        ManagedChannel proxyChannel,
                        ManagedChannel spaceChannel) {
        this.leaseChannel    = leaseChannel;
        this.registryChannel = registryChannel;
        this.proxyChannel    = proxyChannel;
        this.spaceChannel    = spaceChannel;
    }

    public static DjinnClient connect(String host) {
        return connect(host, 9001, 9002, 9003, 9006);
    }

    public static DjinnClient connect(String host, int leasePort, int registryPort,
                                      int proxyPort, int spacePort) {
        ManagedChannel leaseChannel = ManagedChannelBuilder
                .forAddress(host, leasePort).usePlaintext().build();
        ManagedChannel registryChannel = ManagedChannelBuilder
                .forAddress(host, registryPort).usePlaintext().build();
        ManagedChannel proxyChannel = ManagedChannelBuilder
                .forAddress(host, proxyPort).usePlaintext().build();
        ManagedChannel spaceChannel = ManagedChannelBuilder
                .forAddress(host, spacePort).usePlaintext().build();
        return new DjinnClient(leaseChannel, registryChannel, proxyChannel, spaceChannel);
    }

    public LeaseClient    leases()   { return new LeaseClient(leaseChannel); }
    public RegistryClient registry() { return new RegistryClient(registryChannel); }
    public ProxyClient    proxy()    { return new ProxyClient(proxyChannel); }
    public SpaceClient    space()    { return new SpaceClient(spaceChannel); }

    @Override
    public void close() throws InterruptedException {
        leaseChannel.shutdown().awaitTermination(5, TimeUnit.SECONDS);
        registryChannel.shutdown().awaitTermination(5, TimeUnit.SECONDS);
        proxyChannel.shutdown().awaitTermination(5, TimeUnit.SECONDS);
        spaceChannel.shutdown().awaitTermination(5, TimeUnit.SECONDS);
    }
}
