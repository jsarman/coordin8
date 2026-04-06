# hello-coordin8

The smallest end-to-end demo: a Greeter service registered in the Coordin8 Registry, called by clients in three languages. Same service, three SDKs, identical behavior.

For project context see the [root README](../../README.md).

## Layout

```
go/
  proto/greeter.proto         service definition for the example
  gen/                        generated Go stubs
  greeter_service/            the registered Go service
  greeter_client/             a Go client that discovers it via the Registry
  go.mod
java/                         Java client (Gradle)
node/                         Node client (ts-node + ts-proto)
```

The Go subdirectory contains both the **service** and **a client**. The Java and Node subdirectories are clients only — they call whatever Greeter is currently registered, which in the default flow is the Go service.

## Run It

You need a running Djinn. Easiest path:

```bash
docker compose up --build      # Djinn + greeter pre-registered
```

Then call from any language:

```bash
# Go
cd go && go run ./greeter_client/ John

# Java
cd java && ./gradlew run --args="John"

# Node
cd node && npx ts-node src/greeter-client.ts John
```

All three produce: `Hello, John! (from Coordin8 greeter_service)`

## Run Without Docker

```bash
# Terminal 1 — Djinn
mise r djinn

# Terminal 2 — register the greeter
cd go && go run ./greeter_service/

# Terminal 3 — call it
cd go && go run ./greeter_client/ John
```

Kill the service. Run the client again. You get `No Greeter available` — no cleanup, no stale entries, the lease just expired.

## Build

```bash
# from the repo root
make build-examples            # builds Go binary, Java distribution, Node dist
```

## Notes

- The greeter proto is local to the example (`go/proto/greeter.proto`) — it's the *application* protocol, not part of Coordin8 itself
- The clients use `ServiceDiscovery` from each SDK — no IPs or ports in client code
