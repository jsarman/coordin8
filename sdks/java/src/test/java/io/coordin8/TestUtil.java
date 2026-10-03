package io.coordin8;

import io.grpc.BindableService;
import io.grpc.ManagedChannel;
import io.grpc.Server;
import io.grpc.ServerBuilder;
import io.grpc.ServerInterceptor;
import io.grpc.ServerInterceptors;
import io.grpc.inprocess.InProcessChannelBuilder;
import io.grpc.inprocess.InProcessServerBuilder;

import java.util.ArrayList;
import java.util.List;
import java.util.concurrent.TimeUnit;
import java.util.function.BooleanSupplier;

/** Shared helpers: fake servers (in-process or real localhost) and polling. */
final class TestUtil {
    private TestUtil() {}

    /** Owns servers/channels started during a test; close() tears all down. */
    static final class Harness implements AutoCloseable {
        private final List<Server> servers = new ArrayList<>();
        private final List<ManagedChannel> channels = new ArrayList<>();

        /** Start an in-process server and return a channel to it. */
        ManagedChannel inProcess(BindableService... services) throws Exception {
            String name = "test-" + System.nanoTime() + "-" + servers.size();
            InProcessServerBuilder b = InProcessServerBuilder.forName(name);
            for (BindableService s : services) b.addService(s);
            servers.add(b.build().start());
            ManagedChannel ch = InProcessChannelBuilder.forName(name).build();
            channels.add(ch);
            return ch;
        }

        /** Start a real server on an ephemeral localhost port; returns the port. */
        int tcp(ServerInterceptor interceptor, BindableService... services) throws Exception {
            ServerBuilder<?> b = ServerBuilder.forPort(0);
            for (BindableService s : services) {
                b.addService(interceptor == null
                        ? s.bindService()
                        : ServerInterceptors.intercept(s, interceptor));
            }
            Server server = b.build().start();
            servers.add(server);
            return server.getPort();
        }

        int tcp(BindableService... services) throws Exception {
            return tcp(null, services);
        }

        @Override
        public void close() throws Exception {
            for (ManagedChannel c : channels) c.shutdownNow();
            for (Server s : servers) s.shutdownNow();
            for (Server s : servers) s.awaitTermination(5, TimeUnit.SECONDS);
        }
    }

    /** Poll until cond is true or timeout; returns the final value. */
    static boolean await(long timeoutMs, BooleanSupplier cond) throws InterruptedException {
        long deadline = System.nanoTime() + timeoutMs * 1_000_000L;
        while (System.nanoTime() < deadline) {
            if (cond.getAsBoolean()) return true;
            Thread.sleep(25);
        }
        return cond.getAsBoolean();
    }
}
