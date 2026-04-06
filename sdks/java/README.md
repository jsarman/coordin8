# Coordin8 Java SDK

The Java client SDK for the Coordin8 Djinn. Built with Gradle. gRPC stubs are generated from `../../proto/` at build time by the `com.google.protobuf` plugin.

For project context see the [root README](../../README.md). This document covers the Java SDK only.

## Layout

```
build.gradle                  Gradle config — protoc + grpc-java plugin
settings.gradle
src/main/java/io/coordin8/    SDK source (DjinnClient, ServiceDiscovery, ...)
build/generated/source/proto/ generated stubs (after first build)
```

## Public API

```java
var djinn = DjinnClient.connect("localhost");

var discovery = ServiceDiscovery.watch(djinn);
var greeter = discovery.get(GreeterGrpc::newBlockingStub, Map.of("interface", "Greeter"));
```

## Build / Test

```bash
./gradlew build
./gradlew test
./gradlew clean
```

## Regenerating Stubs

The Gradle protobuf plugin compiles `../../proto/coordin8/*.proto` on every build — no manual step. To pick up new proto files just rebuild.

## Gotchas

- The `proto { srcDir }` block lives in the top-level `sourceSets {}` block, **not** inside `protobuf {}`. Plugin 0.9.x doesn't support nesting it there.
- `lease.proto` generates an outer class named `LeaseOuterClass`, not `Lease` — protoc renames the outer class when the filename collides with a message name. Import as `coordin8.LeaseOuterClass.*`.
- Java 21 is required (`mise.toml` pins `openjdk-21`)
