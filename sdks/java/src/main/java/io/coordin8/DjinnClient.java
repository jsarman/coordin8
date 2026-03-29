package io.coordin8;

import io.grpc.ManagedChannel;
import io.grpc.ManagedChannelBuilder;

import java.util.concurrent.TimeUnit;

/**
 * Entry point for all Djinn interactions.
 *
 * <pre>{@code
 * try (DjinnClient djinn = DjinnClient.connect("localhost")) {
 *     LeaseClient leases     = djinn.leases();
 *     RegistryClient registry = djinn.registry();
 * }
 * }</pre>
 */
public class DjinnClient implements AutoCloseable {

    private final ManagedChannel leaseChannel;
    private final ManagedChannel registryChannel;

    private DjinnClient(ManagedChannel leaseChannel, ManagedChannel registryChannel) {
        this.leaseChannel    = leaseChannel;
        this.registryChannel = registryChannel;
    }

    public static DjinnClient connect(String host) {
        return connect(host, 9001, 9002);
    }

    public static DjinnClient connect(String host, int leasePort, int registryPort) {
        ManagedChannel leaseChannel = ManagedChannelBuilder
                .forAddress(host, leasePort)
                .usePlaintext()
                .build();
        ManagedChannel registryChannel = ManagedChannelBuilder
                .forAddress(host, registryPort)
                .usePlaintext()
                .build();
        return new DjinnClient(leaseChannel, registryChannel);
    }

    public LeaseClient leases() {
        return new LeaseClient(leaseChannel);
    }

    public RegistryClient registry() {
        return new RegistryClient(registryChannel);
    }

    @Override
    public void close() throws InterruptedException {
        leaseChannel.shutdown().awaitTermination(5, TimeUnit.SECONDS);
        registryChannel.shutdown().awaitTermination(5, TimeUnit.SECONDS);
    }
}
