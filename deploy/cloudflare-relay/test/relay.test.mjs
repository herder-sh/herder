// The relay Worker in a local workerd (miniflare): whom it lets in, and what it forwards.

import assert from "node:assert/strict";
import { createHmac } from "node:crypto";
import { after, before, test } from "node:test";
import { fileURLToPath } from "node:url";
import { Miniflare } from "miniflare";

const SECRET = "test-relay-secret";

let mf;
/** Where the local runtime listens, for real sockets. */
let base;

before(async () => {
  mf = new Miniflare({
    modules: true,
    scriptPath: fileURLToPath(new URL("../worker.js", import.meta.url)),
    compatibilityDate: "2026-07-01",
    durableObjects: { RELAY: { className: "Relay", useSQLite: true } },
    bindings: { RELAY_SECRET: SECRET },
  });
  base = await mf.ready;
});

after(() => mf?.dispose());

/** A token as the hub signs one: `v1.<claims>.<mac>`. */
function token({ sub, kind, exp = Math.floor(Date.now() / 1000) + 3600, secret = SECRET }) {
  const claims = Buffer.from(JSON.stringify({ sub, kind, exp })).toString("base64url");
  const mac = createHmac("sha256", secret).update(`v1.${claims}`).digest("base64url");
  return `v1.${claims}.${mac}`;
}

const bearer = (claims) => ({ Authorization: `Bearer ${token(claims)}` });
const client = bearer({ sub: "phone-1", kind: "client" });
const host = (sub) => bearer({ sub, kind: "host" });

/** The status the relay answers an upgrade to `path` with. */
async function status(path, headers = {}) {
  const response = await mf.dispatchFetch(new URL(path, base), {
    headers: { Upgrade: "websocket", ...headers },
  });
  return response.status;
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

const bytes = (text) => new TextEncoder().encode(text);
const text = (data) => Buffer.from(data).toString();

test("a request that is not a WebSocket upgrade is refused", async () => {
  const response = await mf.dispatchFetch(new URL("/machines/h1", base));
  assert.equal(response.status, 426);
});

test("no valid token, no entry", async () => {
  const now = Math.floor(Date.now() / 1000);
  assert.equal(await status("/machines/h1"), 401, "no token");
  assert.equal(await status("/machines/h1", { Authorization: "Bearer v1.x.y" }), 401, "malformed");
  assert.equal(
    await status("/machines/h1", bearer({ sub: "phone-1", kind: "client", secret: "other" })),
    401,
    "signed with another secret",
  );
  assert.equal(
    await status("/machines/h1", bearer({ sub: "phone-1", kind: "client", exp: now - 1 })),
    401,
    "expired",
  );
  assert.equal(
    await status("/machines/h1", bearer({ sub: "phone-1", kind: "admin" })),
    401,
    "an unknown kind",
  );
  // A token from another relay of the same shape: claims swapped under a valid mac.
  const [, , mac] = token({ sub: "phone-1", kind: "client" }).split(".");
  const forged = Buffer.from(JSON.stringify({ sub: "h1", kind: "host", exp: now + 60 })).toString(
    "base64url",
  );
  assert.equal(await status("/hosts/h1", { Authorization: `Bearer v1.${forged}.${mac}` }), 401);
});

test("each kind of token opens only its own paths", async () => {
  assert.equal(await status("/hosts/h1", client), 401, "a client is not a machine");
  assert.equal(await status(`/streams/${"0".repeat(32)}`, client), 401, "nor opens streams");
  assert.equal(await status("/hosts/h2", host("h1")), 401, "a machine is only itself");
  assert.equal(await status("/machines/h2", host("h1")), 401, "a machine is not a client");
  assert.equal(await status("/elsewhere/h1", client), 404);
  assert.equal(await status("/machines/h1", { ...client, Origin: "https://evil.test" }), 403);
});

test("a device is refused while its machine is not connected", async () => {
  assert.equal(await status("/machines/offline", client), 503);
});

test("a device's frames reach its machine, held until it opens its end, and back", async () => {
  const machine = await connect("/hosts/h1", host("h1"));
  const app = await connect("/machines/h1", client);
  // Sent before the machine opened its end: held, then delivered first, in order.
  app.socket.send(bytes("hello 1"));
  app.socket.send(bytes("hello 2"));
  const { open: id } = JSON.parse(await machine.next());
  assert.match(id, /^[0-9a-f]{32}$/);
  // Another machine cannot take the stream.
  assert.equal(await status(`/streams/${id}`, host("h2")), 404);
  const end = await connect(`/streams/${id}`, host("h1"));
  assert.equal(text(await end.next()), "hello 1");
  assert.equal(text(await end.next()), "hello 2");
  app.socket.send(bytes("after"));
  assert.equal(text(await end.next()), "after");
  end.socket.send(bytes("from the machine"));
  assert.equal(text(await app.next()), "from the machine");
  // A stream is opened once.
  assert.equal(await status(`/streams/${id}`, host("h1")), 404);

  // Closing one end closes the other.
  end.socket.close(1000, "done");
  await app.closed();
  machine.socket.close(1000, "done");
});

test("streams to one machine do not reach another", async () => {
  const one = await connect("/hosts/h3", host("h3"));
  const two = await connect("/hosts/h4", host("h4"));
  const app = await connect("/machines/h4", client);
  const { open: id } = JSON.parse(await two.next());
  assert.equal(await status(`/streams/${id}`, host("h3")), 404);
  app.socket.close(1000, "done");
  one.socket.close(1000, "done");
  two.socket.close(1000, "done");
});

test("text frames on a stream are refused", async () => {
  const machine = await connect("/hosts/h5", host("h5"));
  const app = await connect("/machines/h5", client);
  const { open: id } = JSON.parse(await machine.next());
  await connect(`/streams/${id}`, host("h5"));
  app.socket.send("plain text");
  assert.equal((await app.closed()).code, 1003);
  machine.socket.close(1000, "done");
});

test("a machine that connects again replaces its old connection", async () => {
  const old = await connect("/hosts/h6", host("h6"));
  const fresh = await connect("/hosts/h6", host("h6"));
  assert.equal((await old.closed()).code, 1000);
  const app = await connect("/machines/h6", client);
  assert.ok(JSON.parse(await fresh.next()).open);
  app.socket.close(1000, "done");
  fresh.socket.close(1000, "done");
});
