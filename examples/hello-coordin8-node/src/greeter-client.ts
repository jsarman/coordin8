import * as grpc from "@grpc/grpc-js";
import { DjinnClient } from "@coordin8/sdk";
import { GreeterServiceClient } from "../gen/greeter";

async function main() {
  const name = process.argv[2] ?? "Coordin8 (Node)";

  // ── 1. Connect to the Djinn ──────────────────────────────────────────────
  const djinn = DjinnClient.connect("localhost");

  try {
    // ── 2. Look up a Greeter — no address, just a template ──────────────────
    console.log("Looking up Greeter in Registry...");
    const cap = await djinn.registry().lookup({ interface: "Greeter" });

    if (!cap) {
      console.log("  ✗ No Greeter available");
      process.exit(1);
    }

    console.log(`  ✓ Found: ${cap.interfaceName}  (capability=${cap.capabilityId})`);

    const host = cap.transport?.config["host"];
    const port = cap.transport?.config["port"];
    console.log(`  ✓ Transport: ${cap.transport?.type} @ ${host}:${port}`);

    // ── 3. Connect to the service using the transport descriptor ────────────
    const channel = new grpc.Channel(
      `${host}:${port}`,
      grpc.credentials.createInsecure(),
      {}
    );
    const greeter = new GreeterServiceClient(
      `${host}:${port}`,
      grpc.credentials.createInsecure(),
      { channelOverride: channel }
    );

    // ── 4. Call Hello ────────────────────────────────────────────────────────
    const resp = await new Promise<{ message: string }>((resolve, reject) => {
      greeter.hello({ name }, (err, res) => {
        if (err || !res) return reject(err);
        resolve(res);
      });
    });

    console.log(`\n  → ${resp.message}`);
    channel.close();
  } finally {
    djinn.close();
  }
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
