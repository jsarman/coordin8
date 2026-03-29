import * as grpc from "@grpc/grpc-js";
import { DjinnClient, ServiceDiscovery } from "@coordin8/sdk";
import { GreeterServiceClient } from "../gen/greeter";

async function main() {
  const name = process.argv[2] ?? "Coordin8 (Node)";

  const djinn = DjinnClient.connect("localhost");
  const discovery = ServiceDiscovery.watch(djinn);

  try {
    const greeter = await discovery.get(
      (addr) => new GreeterServiceClient(addr, grpc.credentials.createInsecure()),
      { interface: "Greeter" }
    );

    const resp = await new Promise<{ message: string }>((resolve, reject) => {
      greeter.hello({ name }, (err, res) => {
        if (err || !res) return reject(err);
        resolve(res);
      });
    });

    console.log(resp.message);
  } finally {
    await discovery.close();
    djinn.close();
  }
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
