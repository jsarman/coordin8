package io.coordin8;

import coordin8.RegistryServiceGrpc;
import coordin8.Registry.RegisterRequest;
import coordin8.Registry.RegisterResponse;
import coordin8.Registry.ModifyAttrsRequest;
import coordin8.Registry.LookupRequest;
import coordin8.Registry.RegistryWatchRequest;
import coordin8.Registry.RegistryEvent;
import coordin8.Registry.Capability;
import io.grpc.ManagedChannel;
import io.grpc.StatusRuntimeException;
import io.grpc.stub.StreamObserver;

import java.util.ArrayList;
import java.util.Iterator;
import java.util.List;
import java.util.Map;
import java.util.Optional;
import java.util.function.Consumer;

public class RegistryClient {

    private final RegistryServiceGrpc.RegistryServiceBlockingStub stub;
    private final RegistryServiceGrpc.RegistryServiceStub asyncStub;

    RegistryClient(ManagedChannel channel) {
        this.stub = RegistryServiceGrpc.newBlockingStub(channel);
        this.asyncStub = RegistryServiceGrpc.newStub(channel);
    }

    /**
     * Register a service capability with the Djinn.
     *
     * @param interfaceName the interface name (e.g., "Greeter")
     * @param attrs         service attributes
     * @param ttlSeconds    requested lease TTL in seconds
     * @param transport     optional transport descriptor (null if not needed)
     * @return the registration result with capability ID and lease
     */
    public RegisterResult register(String interfaceName, Map<String, String> attrs,
                                   long ttlSeconds, TransportDescriptor transport) {
        return register(interfaceName, attrs, ttlSeconds, transport, null);
    }

    /**
     * Register or re-register a service capability with the Djinn.
     *
     * @param interfaceName the interface name
     * @param attrs         service attributes
     * @param ttlSeconds    requested lease TTL in seconds
     * @param transport     optional transport descriptor
     * @param capabilityId  if non-null, updates an existing registration in-place
     * @return the registration result with capability ID and lease
     */
    public RegisterResult register(String interfaceName, Map<String, String> attrs,
                                   long ttlSeconds, TransportDescriptor transport,
                                   String capabilityId) {
        RegisterRequest.Builder req = RegisterRequest.newBuilder()
                .setInterface(interfaceName)
                .setTtlSeconds(ttlSeconds);
        if (attrs != null) {
            req.putAllAttrs(attrs);
        }
        if (transport != null) {
            req.setTransport(coordin8.Registry.TransportDescriptor.newBuilder()
                    .setType(transport.type())
                    .putAllConfig(transport.config())
                    .build());
        }
        if (capabilityId != null) {
            req.setCapabilityId(capabilityId);
        }

        RegisterResponse resp = stub.register(req.build());
        String leaseId = resp.hasLease() ? resp.getLease().getLeaseId() : null;
        long grantedTtl = resp.hasLease() ? resp.getLease().getTtlSeconds() : 0;
        return new RegisterResult(resp.getCapabilityId(), leaseId, grantedTtl);
    }

    /**
     * Modify attributes on an existing registration without re-registering.
     *
     * @param capabilityId the capability to modify
     * @param addAttrs     attributes to add or update (null to skip)
     * @param removeAttrs  attribute keys to remove (null to skip)
     * @return the updated capability
     */
    public CapabilityRecord modifyAttrs(String capabilityId,
                                        Map<String, String> addAttrs,
                                        List<String> removeAttrs) {
        ModifyAttrsRequest.Builder req = ModifyAttrsRequest.newBuilder()
                .setCapabilityId(capabilityId);
        if (addAttrs != null) {
            req.putAllAddAttrs(addAttrs);
        }
        if (removeAttrs != null) {
            req.addAllRemoveAttrs(removeAttrs);
        }
        Capability cap = stub.modifyAttrs(req.build());
        return toRecord(cap);
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

    /**
     * Watch for registry events matching the template.
     * The callback is invoked on a gRPC thread for each event.
     *
     * @param template  attribute template to filter events
     * @param onEvent   callback for each registry event
     * @param onError   callback for errors (stream will be closed)
     */
    public void watch(Map<String, String> template,
                      Consumer<RegistryEventRecord> onEvent,
                      Consumer<Throwable> onError) {
        RegistryWatchRequest req = RegistryWatchRequest.newBuilder()
                .putAllTemplate(template)
                .build();
        asyncStub.watch(req, new StreamObserver<RegistryEvent>() {
            @Override
            public void onNext(RegistryEvent evt) {
                String type = switch (evt.getType()) {
                    case EXPIRED -> "expired";
                    case MODIFIED -> "modified";
                    default -> "registered";
                };
                CapabilityRecord cap = evt.hasCapability() ? toRecord(evt.getCapability()) : null;
                onEvent.accept(new RegistryEventRecord(type, cap));
            }

            @Override
            public void onError(Throwable t) {
                onError.accept(t);
            }

            @Override
            public void onCompleted() {}
        });
    }

    static CapabilityRecord toRecord(Capability c) {
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

    public record RegisterResult(
            String capabilityId,
            String leaseId,
            long grantedTtlSeconds
    ) {}

    public record RegistryEventRecord(
            String type,
            CapabilityRecord capability
    ) {}
}
