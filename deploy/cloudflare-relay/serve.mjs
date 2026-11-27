// Runs the relay Worker in a local workerd (miniflare) for herder's Rust end-to-end tests:
// `node serve.mjs <relay secret>` prints the URL it listens on, then serves until stdin closes.

import { fileURLToPath } from "node:url";
import { Miniflare } from "miniflare";

const secret = process.argv[2];
if (!secret) {
  console.error("usage: node serve.mjs <relay secret>");
  process.exit(2);
}
const mf = new Miniflare({
  modules: true,
  scriptPath: fileURLToPath(new URL("./worker.js", import.meta.url)),
  compatibilityDate: "2026-07-01",
  durableObjects: { RELAY: { className: "Relay", useSQLite: true } },
  bindings: { RELAY_SECRET: secret },
  host: "127.0.0.1",
  port: 0,
});
console.log(String(await mf.ready));
process.stdin.resume();
process.stdin.on("end", async () => {
  await mf.dispose();
  process.exit(0);
});
