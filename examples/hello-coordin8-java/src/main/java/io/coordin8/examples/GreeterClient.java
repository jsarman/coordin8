package io.coordin8.examples;

import io.coordin8.DjinnClient;
import io.coordin8.RegistryClient.CapabilityRecord;
import io.grpc.ManagedChannel;
import io.grpc.ManagedChannelBuilder;

import hello.GreeterServiceGrpc;
import hello.Greeter.HelloRequest;
import hello.Greeter.HelloResponse;

import java.util.Map;
import java.util.Optional;

/**
 * Java client for the Hello Coordin8 demo.
 *
 * Finds the Greeter service in the Djinn Registry by template — no hardcoded
 * host or port in this file. The transport descriptor comes back from the
 * Registry and we connect using that.
 *
 * The Greeter service itself is the Go greeter_service from Phase 3.
 * Different language, same Djinn, same wire protocol.
 */
public class GreeterClient {

    public static void main(String[] args) throws Exception {
        String name = args.length > 0 ? args[0] : "Coordin8 (Java)";

        // ── 1. Connect to the Djinn ───────────────────────────────────────────
        try (DjinnClient djinn = DjinnClient.connect("localhost")) {

            // ── 2. Look up a Greeter — no address, just a template ────────────
            System.out.println("Looking up Greeter in Registry...");
            Optional<CapabilityRecord> result = djinn.registry().lookup(Map.of(
                    "interface", "Greeter"
            ));

            if (result.isEmpty()) {
                System.out.println("  ✗ No Greeter available");
                System.exit(1);
            }

            CapabilityRecord cap = result.get();
            System.out.printf("  ✓ Found: %s  (capability=%s)%n",
                    cap.interfaceName(), cap.capabilityId());

            String host = cap.transport().config().get("host");
            String port = cap.transport().config().get("port");
            System.out.printf("  ✓ Transport: %s @ %s:%s%n",
                    cap.transport().type(), host, port);

            // ── 3. Connect to the service using the transport descriptor ──────
            ManagedChannel channel = ManagedChannelBuilder
                    .forAddress(host, Integer.parseInt(port))
                    .usePlaintext()
                    .build();

            try {
                // ── 4. Call Hello ─────────────────────────────────────────────
                GreeterServiceGrpc.GreeterServiceBlockingStub greeter =
                        GreeterServiceGrpc.newBlockingStub(channel);

                HelloResponse resp = greeter.hello(
                        HelloRequest.newBuilder().setName(name).build()
                );

                System.out.printf("%n  → %s%n", resp.getMessage());
            } finally {
                channel.shutdown();
            }
        }
    }
}
