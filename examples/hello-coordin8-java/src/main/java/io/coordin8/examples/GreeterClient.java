package io.coordin8.examples;

import io.coordin8.DjinnClient;
import io.grpc.ManagedChannelBuilder;

import hello.GreeterServiceGrpc;
import hello.Greeter.HelloRequest;
import hello.Greeter.HelloResponse;

import java.util.Map;

/**
 * Java client for the Hello Coordin8 demo — proxy edition.
 *
 * One call to proxyStub: the Djinn finds the service, opens a local
 * forwarding port, and hands back a ready stub. No host, no port,
 * no transport descriptor in this file.
 */
public class GreeterClient {

    public static void main(String[] args) throws Exception {
        String name = args.length > 0 ? args[0] : "Coordin8 (Java)";

        try (DjinnClient djinn = DjinnClient.connect("localhost")) {

            System.out.println("Opening proxy to Greeter...");
            GreeterServiceGrpc.GreeterServiceBlockingStub greeter =
                    djinn.proxy().proxyStub(
                            channel -> GreeterServiceGrpc.newBlockingStub(channel),
                            Map.of("interface", "Greeter")
                    );

            HelloResponse resp = greeter.hello(
                    HelloRequest.newBuilder().setName(name).build()
            );

            System.out.printf("%n  → %s%n", resp.getMessage());
        }
    }
}
