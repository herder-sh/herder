// The relay Worker in a local workerd (miniflare): who it lets in, and what it forwards.

import assert from "node:assert/strict";
import { createHash, generateKeyPairSync, sign } from "node:crypto";
import { after, before, test } from "node:test";
import { fileURLToPath } from "node:url";
import { Miniflare } from "miniflare";

const HUB_KEY = "test-hub-key";
const URL_BASE = "http://relay.test";

let mf;
/** Where the local runtime listens, for real sockets. */
let base;

before(async () => {
  mf = new Miniflare({
    modules: true,
    scriptPath: fileURLToPath(new URL("../worker.js", import.meta.url)),
    compatibilityDate: "2026-07-01",
    durableObjects: { RELAY: { className: "Relay", useSQLite: true } },
    bindings: { HUB_KEY },
  });
  base = await mf.ready;
});

after(() => mf?.dispose());

/** A device: a stand-in certificate holding an Ed25519 key, and its fingerprint. */
function device() {
  const { publicKey, privateKey } = generateKeyPairSync("ed25519");
  const spki = publicKey.export({ type: "spki", format: "der" });
  // The relay reads the key from the certificate by its encoding, wherever it sits.
  const cert = Buffer.concat([Buffer.from("30820100", "hex"), spki, Buffer.alloc(80, 7)]);
  const fingerprint = createHash("sha256").update(cert).digest("hex");
  return { cert, fingerprint, privateKey };
}

function signed(dev, { time = Math.floor(Date.now() / 1000), host = base.host } = {}) {
  const message = Buffer.from(`herder-relay\n${host}\n${time}`);
  return {
    "x-herder-device": dev.cert.toString("base64"),
    "x-herder-time": String(time),
    "x-herder-signature": sign(null, message, dev.privateKey).toString("base64"),
  };
}

async function upgrade(path, headers = {}) {
  return mf.dispatchFetch(`${URL_BASE}${path}`, { headers: { Upgrade: "websocket", ...headers } });
}

/** A WebSocket to the relay over a real socket, once open, queueing what it receives. */
async function connect(path, headers = {}) {
  const socket = new WebSocket(new URL(path, base).href.replace(/^http/, "ws"), { headers });
  socket.binaryType = "arraybuffer";
  const inbox = [];
  const waiters = [];
  let closed = null;
  const closeWaiters = [];
  socket.addEventListener("message", (event) => {
    const waiter = waiters.shift();
    if (waiter) waiter(event.data);
    else inbox.push(event.data);
  });
  socket.addEventListener("close", (event) => {
    closed = event;
    for (const waiter of closeWaiters.splice(0)) waiter(event);
  });
  await new Promise((resolve, reject) => {
    socket.addEventListener("open", resolve);
    socket.addEventListener("error", () => reject(new Error(`upgrade to ${path} refused`)));
  });
  return {
    socket,
    next: () =>
      inbox.length
        ? Promise.resolve(inbox.shift())
        : new Promise((resolve) => waiters.push(resolve)),
    closed: () => (closed ? Promise.resolve(closed) : new Promise((r) => closeWaiters.push(r))),
  };
}

const hubAuth = { Authorization: `Bearer ${HUB_KEY}` };

async function hub(devices) {
  const control = await connect("/hub", hubAuth);
  control.socket.send(JSON.stringify({ devices }));
  return control;
}

function text(data) {
  return Buffer.from(data).toString();
}

test("a request that is not a WebSocket upgrade is refused", async () => {
  const response = await mf.dispatchFetch(`${URL_BASE}/`);
  assert.equal(response.status, 426);
});

test("only the hub's key opens the hub's end", async () => {
  assert.equal((await upgrade("/hub")).status, 401);
  assert.equal((await upgrade("/hub", { Authorization: "Bearer nope" })).status, 401);
  const accept = `/accept/${"0".repeat(32)}`;
  assert.equal((await upgrade(accept, { Authorization: "Bearer nope" })).status, 401);
  assert.equal((await upgrade(accept, hubAuth)).status, 404);
});

test("devices that are not paired, or sign wrongly, are refused before the hub hears of them", async () => {
  const paired = device();
  const stranger = device();
  const control = await hub([paired.fingerprint]);
  // The allowlist is kept before the next request is served.
  await new Promise((resolve) => setTimeout(resolve, 50));

  assert.equal((await upgrade("/")).status, 401, "no headers");
  assert.equal((await upgrade("/", signed(stranger, { host: "relay.test" }))).status, 401, "not paired");
  const stale = Math.floor(Date.now() / 1000) - 3600;
  assert.equal((await upgrade("/", signed(paired, { time: stale, host: "relay.test" }))).status, 401, "stale");
  assert.equal(
    (await upgrade("/", signed(paired, { host: "elsewhere.test" }))).status,
    401,
    "signed for another relay",
  );
  const forged = { ...signed(stranger, { host: "relay.test" }), "x-herder-device": paired.cert.toString("base64") };
  assert.equal((await upgrade("/", forged)).status, 401, "another device's certificate");
  assert.equal(
    (await upgrade("/", { ...signed(paired, { host: "relay.test" }), Origin: "https://evil.test" })).status,
    403,
    "a browser",
  );

  const ok = await connect("/", signed(paired));
  const asked = JSON.parse(await control.next());
  assert.match(asked.open, /^[0-9a-f]{32}$/);
  ok.socket.close(1000, "done");
  control.socket.close(1000, "done");
});

test("a paired device is refused while the hub is not connected", async () => {
  const paired = device();
  const control = await hub([paired.fingerprint]);
  control.socket.close(1000, "bye");
  await control.closed();
  await new Promise((resolve) => setTimeout(resolve, 50));
  assert.equal((await upgrade("/", signed(paired, { host: "relay.test" }))).status, 503);
});

test("a device's frames reach the hub's end, held until it opens, and back", async () => {
  const paired = device();
  const control = await hub([paired.fingerprint]);
  await new Promise((resolve) => setTimeout(resolve, 50));
  const app = await connect("/", signed(paired));
  // Sent before the hub opened its end: held, then delivered first, in order.
  app.socket.send(new TextEncoder().encode("hello 1"));
  app.socket.send(new TextEncoder().encode("hello 2"));
  const { open: id } = JSON.parse(await control.next());
  const end = await connect(`/accept/${id}`, hubAuth);
  assert.equal(text(await end.next()), "hello 1");
  assert.equal(text(await end.next()), "hello 2");
  app.socket.send(new TextEncoder().encode("after"));
  assert.equal(text(await end.next()), "after");
  end.socket.send(new TextEncoder().encode("from the hub"));
  assert.equal(text(await app.next()), "from the hub");
  // A stream is opened once.
  assert.equal((await upgrade(`/accept/${id}`, hubAuth)).status, 404);

  // Closing one end closes the other.
  end.socket.close(1000, "done");
  await app.closed();
  control.socket.close(1000, "done");
});

test("text frames on a stream are refused", async () => {
  const paired = device();
  const control = await hub([paired.fingerprint]);
  await new Promise((resolve) => setTimeout(resolve, 50));
  const app = await connect("/", signed(paired));
  const { open: id } = JSON.parse(await control.next());
  const end = await connect(`/accept/${id}`, hubAuth);
  app.socket.send("plain text");
  assert.equal((await app.closed()).code, 1003);
  await end.closed();
  control.socket.close(1000, "done");
});

test("a device the hub unpairs loses its streams and gets no new one", async () => {
  const kept = device();
  const revoked = device();
  const control = await hub([kept.fingerprint, revoked.fingerprint]);
  await new Promise((resolve) => setTimeout(resolve, 50));
  const keptApp = await connect("/", signed(kept));
  const revokedApp = await connect("/", signed(revoked));
  await control.next();
  await control.next();
  control.socket.send(JSON.stringify({ devices: [kept.fingerprint] }));
  assert.equal((await revokedApp.closed()).code, 1008);
  assert.equal((await upgrade("/", signed(revoked, { host: "relay.test" }))).status, 401);
  keptApp.socket.close(1000, "done");
  control.socket.close(1000, "done");
});
