package io.coordin8.examples;

import io.coordin8.DjinnClient;
import io.coordin8.ServiceDiscovery;

import hello.GreeterServiceGrpc;
import hello.Greeter.HelloRequest;
import hello.Greeter.HelloResponse;

import java.util.Map;

public class GreeterClient {

    public static void main(String[] args) throws Exception {
        String name = args.length > 0 ? args[0] : "Coordin8 (Java)";

        String registryAddr = System.getenv("COORDIN8_REGISTRY");
        if (registryAddr == null || registryAddr.isEmpty()) {
            registryAddr = "localhost:9002";
        }

        try (DjinnClient djinn = DjinnClient.connect(registryAddr);
             ServiceDiscovery discovery = ServiceDiscovery.watch(djinn)) {

            var greeter = discovery.get(
                    GreeterServiceGrpc::newBlockingStub,
                    Map.of("interface", "Greeter")
            );

            HelloResponse resp = greeter.hello(
                    HelloRequest.newBuilder().setName(name).build()
            );

            System.out.println(resp.getMessage());
        }
    }
}
