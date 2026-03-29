package io.coordin8;

import coordin8.RegistryServiceGrpc;
import coordin8.Registry.LookupRequest;
import coordin8.Registry.Capability;
import io.grpc.ManagedChannel;
import io.grpc.StatusRuntimeException;

import java.util.ArrayList;
import java.util.Iterator;
import java.util.List;
import java.util.Map;
import java.util.Optional;

public class RegistryClient {

    private final RegistryServiceGrpc.RegistryServiceBlockingStub stub;

    RegistryClient(ManagedChannel channel) {
        this.stub = RegistryServiceGrpc.newBlockingStub(channel);
    }

    /**
     * Look up the first capability matching the template.
     * Template semantics: missing fields match anything,
     * values prefixed "contains:" or "starts_with:" use those operators.
     */
    public Optional<CapabilityRecord> lookup(Map<String, String> template) {
        try {
            Capability cap = stub.lookup(LookupRequest.newBuilder()
                    .putAllTemplate(template)
                    .build());
            return Optional.of(toRecord(cap));
        } catch (StatusRuntimeException e) {
            if (e.getStatus().getCode() == io.grpc.Status.Code.NOT_FOUND) {
                return Optional.empty();
            }
            throw e;
        }
    }

    /** Look up all capabilities matching the template. */
    public List<CapabilityRecord> lookupAll(Map<String, String> template) {
        Iterator<Capability> it = stub.lookupAll(LookupRequest.newBuilder()
                .putAllTemplate(template)
                .build());
        List<CapabilityRecord> results = new ArrayList<>();
        it.forEachRemaining(cap -> results.add(toRecord(cap)));
        return results;
    }

    private static CapabilityRecord toRecord(Capability c) {
        TransportDescriptor transport = null;
        if (c.hasTransport()) {
            transport = new TransportDescriptor(
                    c.getTransport().getType(),
                    c.getTransport().getConfigMap()
            );
        }
        return new CapabilityRecord(
                c.getCapabilityId(),
                c.getInterface(),
                c.getAttrsMap(),
                transport
        );
    }

    public record TransportDescriptor(
            String type,
            Map<String, String> config
    ) {}

    public record CapabilityRecord(
            String capabilityId,
            String interfaceName,
            Map<String, String> attrs,
            TransportDescriptor transport
    ) {}
}
