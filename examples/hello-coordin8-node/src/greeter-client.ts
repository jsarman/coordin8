import * as grpc from "@grpc/grpc-js";
import { DjinnClient } from "@coordin8/sdk";
import { GreeterServiceClient } from "../gen/greeter";

async function main() {
  const name = process.argv[2] ?? "Coordin8 (Node)";

  const djinn = DjinnClient.connect("localhost");

  try {
    // One call. No host, no port, no channel wiring. The Djinn handles it.
    console.log("Opening proxy to Greeter...");
    const greeter = await djinn.proxy().proxyClient(
      (addr) => new GreeterServiceClient(addr, grpc.credentials.createInsecure()),
      { interface: "Greeter" }
    );

    const resp = await new Promise<{ message: string }>((resolve, reject) => {
      greeter.hello({ name }, (err, res) => {
        if (err || !res) return reject(err);
        resolve(res);
      });
    });

    console.log(`\n  → ${resp.message}`);
  } finally {
    djinn.close();
  }
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
