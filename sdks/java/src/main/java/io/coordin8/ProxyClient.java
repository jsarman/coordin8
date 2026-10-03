package io.coordin8;

import coordin8.ProxyServiceGrpc;
import coordin8.Proxy.OpenRequest;
import coordin8.Proxy.ReleaseRequest;
import coordin8.Proxy.ProxyHandle;
import io.grpc.ManagedChannel;
import io.grpc.ManagedChannelBuilder;
import io.grpc.Status;
import io.grpc.StatusRuntimeException;

import java.io.Closeable;
import java.io.IOException;
import java.util.Map;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.function.Function;

public class ProxyClient {

    /** Lease TTL requested by {@link #open(Map)}; renewed in the background until release. */
    public static final long DEFAULT_PROXY_TTL_SECONDS = 30;

    private final ProxyServiceGrpc.ProxyServiceBlockingStub stub;
    /** LeaseService is mounted on Proxy's own channel (Proxy is a Landlord). */
    private final LeaseClient leases;

    ProxyClient(ManagedChannel channel) {
        this.stub = ProxyServiceGrpc.newBlockingStub(channel);
        this.leases = LeaseClient.wrap(channel);
    }

    /**
     * Ask the Djinn to open a local TCP forwarding port for the given template.
     * The proxy is leased ({@link #DEFAULT_PROXY_TTL_SECONDS}); the SDK renews
     * the lease in the background until {@link ProxyHandleRecord#close()}.
     */
    public ProxyHandleRecord open(Map<String, String> template) {
        return open(template, DEFAULT_PROXY_TTL_SECONDS);
    }

    /** Like {@link #open(Map)} with an explicit lease TTL (0 = server's preferred TTL). */
    public ProxyHandleRecord open(Map<String, String> template, long ttlSeconds) {
        ProxyHandle handle = stub.open(OpenRequest.newBuilder()
                .putAllTemplate(template)
                .setTtlSeconds(ttlSeconds)
                .build());
        AtomicBoolean lost = new AtomicBoolean(false);
        Closeable keepAlive = () -> { };
        LeaseClient.LeaseRecord lease = null;
        if (handle.hasLease()) {
            lease = LeaseClient.toRecord(handle.getLease());
            long granted = lease.ttlSeconds() > 0 ? lease.ttlSeconds() : ttlSeconds;
            keepAlive = leases.keepAlive(lease.leaseId(), granted, err -> {
                if (err instanceof StatusRuntimeException sre) {
                    Status.Code code = sre.getStatus().getCode();
                    if (code == Status.Code.NOT_FOUND || code == Status.Code.FAILED_PRECONDITION) {
                        lost.set(true);
                    }
                }
            });
        }
        return new ProxyHandleRecord(handle.getProxyId(), handle.getLocalPort(), this, lease, keepAlive, lost);
    }

    /** Release a proxy on the Djinn. */
    public void release(String proxyId) {
        stub.release(ReleaseRequest.newBuilder().setProxyId(proxyId).build());
    }

    /**
     * One-liner: open a proxy, build a ManagedChannel to it, pass it to
     * the stub factory, and return the stub. The returned stub is ready to use.
     *
     * <p>The caller is responsible for shutting down the channel and closing
     * the proxy. For a managed lifecycle, use {@link #open} directly.
     *
     * <pre>{@code
     * var greeter = djinn.proxy().proxyStub(
     *     GreeterServiceGrpc::newBlockingStub,
     *     Map.of("interface", "Greeter")
     * );
     * }</pre>
     */
    public <T> T proxyStub(Function<ManagedChannel, T> factory, Map<String, String> template) {
        ProxyHandleRecord handle = open(template);
        ManagedChannel channel = ManagedChannelBuilder
                .forAddress("localhost", handle.localPort())
                .usePlaintext()
                .build();
        return factory.apply(channel);
    }

    /**
     * @param lease     the proxy's lease (grantor = the Proxy itself)
     * @param lost      set once keep-alive learns the Djinn reclaimed the proxy;
     *                  the proxy must be reopened
     */
    public record ProxyHandleRecord(
            String proxyId,
            int localPort,
            ProxyClient client,
            LeaseClient.LeaseRecord lease,
            Closeable keepAlive,
            AtomicBoolean lost
    ) implements AutoCloseable {
        /** Stops lease renewal and releases the proxy (which cancels its lease). */
        @Override
        public void close() {
            try {
                keepAlive.close();
            } catch (IOException ignored) {
                // keep-alive stop never actually throws
            }
            try {
                client.release(proxyId);
            } catch (StatusRuntimeException e) {
                // Already reclaimed (lease expired) — nothing left to release.
                if (e.getStatus().getCode() != Status.Code.NOT_FOUND) throw e;
            }
        }
    }
}
