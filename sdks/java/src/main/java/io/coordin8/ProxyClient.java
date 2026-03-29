package io.coordin8;

import coordin8.ProxyServiceGrpc;
import coordin8.Proxy.OpenRequest;
import coordin8.Proxy.ReleaseRequest;
import coordin8.Proxy.ProxyHandle;
import io.grpc.ManagedChannel;
import io.grpc.ManagedChannelBuilder;

import java.util.Map;
import java.util.function.Function;

public class ProxyClient {

    private final ProxyServiceGrpc.ProxyServiceBlockingStub stub;

    ProxyClient(ManagedChannel channel) {
        this.stub = ProxyServiceGrpc.newBlockingStub(channel);
    }

    /**
     * Ask the Djinn to open a local TCP forwarding port for the given template.
     * Call {@link ProxyHandleRecord#close()} when done.
     */
    public ProxyHandleRecord open(Map<String, String> template) {
        ProxyHandle handle = stub.open(OpenRequest.newBuilder()
                .putAllTemplate(template)
                .build());
        return new ProxyHandleRecord(handle.getProxyId(), handle.getLocalPort(), this);
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

    public record ProxyHandleRecord(
            String proxyId,
            int localPort,
            ProxyClient client
    ) implements AutoCloseable {
        @Override
        public void close() {
            client.release(proxyId);
        }
    }
}
