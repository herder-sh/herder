// herder's relay: a Cloudflare Worker and Durable Object that herder deploys to its owner's
// own Cloudflare account, so devices reach a hub that has no public address
// (docs/relay-worker.md).
//
// The hub keeps one WebSocket open to the relay (`/hub`). A device that opens a WebSocket to
// the relay is told to the hub, which opens a second WebSocket for it (`/accept/<id>`); the
// relay then copies every binary frame from each of the two to the other. Inside those frames
// the device and the hub run their own TLS, so the relay forwards ciphertext only.
//
// Nobody else gets in. The hub proves itself with the key it set when it deployed the relay.
// A device signs the time and the relay's host with the key of its herder device certificate,
// and that certificate must be one the hub told the relay it has paired.
//
// Every socket is accepted through the hibernation API, so the relay sleeps between frames,
// and an idle hub connection costs nothing.

import { DurableObject } from "cloudflare:workers";

/** How far a device's clock may be from Cloudflare's, in seconds. */
const SKEW = 300;

/** How long a device's stream waits for the hub to open its end, in milliseconds. */
const HUB_WAIT = 10_000;

/** Frames a device may send before the hub opened its end: its TLS's first flight. */
const MAX_HELD = 64;

/** An Ed25519 SubjectPublicKeyInfo up to its 32 key bytes. */
const ED25519_SPKI = Uint8Array.of(
  0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
);

export default {
  fetch(request, env) {
    if (request.headers.get("Upgrade")?.toLowerCase() !== "websocket") {
      return refuse(426, "herder's relay takes WebSocket upgrades only");
    }
    // One relay per account: its Durable Object is the hub's meeting point.
    return env.RELAY.get(env.RELAY.idFromName("hub")).fetch(request);
  },
};

export class Relay extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    /** Devices' streams the hub has not opened its end of yet, by id. */
    this.waiting = new Map();
    /** The paired devices' certificate fingerprints, once loaded. */
    this.devices = null;
  }

  async fetch(request) {
    if (request.headers.has("Origin")) {
      return refuse(403, "browsers may not connect");
    }
    const path = new URL(request.url).pathname;
    if (path === "/hub") {
      if (!(await this.isHub(request))) return refuse(401, "not the hub's key");
      for (const old of this.ctx.getWebSockets("hub")) {
        close(old, 1000, "the hub connected again");
      }
      return this.accept(["hub"], () => {});
    }
    const accept = path.match(/^\/accept\/([0-9a-f]{32})$/);
    if (accept) {
      if (!(await this.isHub(request))) return refuse(401, "not the hub's key");
      const id = accept[1];
      const held = this.waiting.get(id);
      if (!held) return refuse(404, "no device waits for this stream");
      this.waiting.delete(id);
      clearTimeout(held.timer);
      return this.accept([`h:${id}`], (socket) => {
        for (const frame of held.frames) socket.send(frame);
      });
    }
    const device = await this.device(request);
    if (!device) {
      return refuse(401, "not a device paired with this relay's hub");
    }
    const [hub] = this.ctx.getWebSockets("hub");
    if (!hub) return refuse(503, "the hub is not connected to this relay");
    const id = hex(crypto.getRandomValues(new Uint8Array(16)));
    return this.accept(["device", `c:${id}`, `d:${device}`], (socket) => {
      const timer = setTimeout(() => {
        this.waiting.delete(id);
        close(socket, 1011, "the hub did not open its end");
      }, HUB_WAIT);
      this.waiting.set(id, { frames: [], timer });
      hub.send(
        JSON.stringify({ open: id, client: request.headers.get("CF-Connecting-IP") }),
      );
    });
  }

  /** Answers the upgrade with a socket tagged `tags`, after `then` ran on it. */
  accept(tags, then) {
    const [client, server] = Object.values(new WebSocketPair());
    this.ctx.acceptWebSocket(server, tags);
    then(server);
    return new Response(null, { status: 101, webSocket: client });
  }

  async webSocketMessage(socket, message) {
    const tags = this.ctx.getTags(socket);
    if (tags.includes("hub")) {
      if (typeof message !== "string") return;
      let devices;
      try {
        ({ devices } = JSON.parse(message));
      } catch {
        return;
      }
      if (Array.isArray(devices)) await this.allow(devices);
      return;
    }
    if (typeof message === "string") {
      this.gone(socket, 1003, "the relay carries binary frames only");
      return;
    }
    const [end, id] = stream(tags);
    if (end === "c") {
      const [peer] = this.ctx.getWebSockets(`h:${id}`);
      if (peer) return peer.send(message);
      const held = this.waiting.get(id);
      if (held && held.frames.length < MAX_HELD) return held.frames.push(message);
      this.gone(socket, 1011, "the hub did not open its end");
    } else if (end === "h") {
      const [peer] = this.ctx.getWebSockets(`c:${id}`);
      if (peer) peer.send(message);
      else this.gone(socket, 1000, "the device is gone");
    }
  }

  webSocketClose(socket, code, reason) {
    this.gone(socket, code, reason);
  }

  webSocketError(socket) {
    this.gone(socket, 1011, "the other end broke");
  }

  /** Closes `socket` and, if it is an end of a stream, the stream's other end. */
  gone(socket, code, reason) {
    close(socket, sendable(code), reason);
    const [end, id] = stream(this.ctx.getTags(socket));
    if (!end) return;
    const peer = end === "c" ? `h:${id}` : `c:${id}`;
    for (const other of this.ctx.getWebSockets(peer)) {
      close(other, sendable(code), reason);
    }
    const held = this.waiting.get(id);
    if (held) {
      clearTimeout(held.timer);
      this.waiting.delete(id);
    }
  }

  /** Keeps `devices` as the paired devices, closing the streams of any other. */
  async allow(devices) {
    this.devices = new Set(devices.filter((device) => /^[0-9a-f]{64}$/.test(device)));
    await this.ctx.storage.put("devices", [...this.devices]);
    for (const socket of this.ctx.getWebSockets("device")) {
      const tag = this.ctx.getTags(socket).find((tag) => tag.startsWith("d:"));
      if (!tag || !this.devices.has(tag.slice(2))) {
        this.gone(socket, 1008, "the device is no longer paired");
      }
    }
  }

  async paired() {
    if (!this.devices) {
      this.devices = new Set((await this.ctx.storage.get("devices")) ?? []);
    }
    return this.devices;
  }

  /** Whether the request carries the key the hub deployed the relay with. */
  async isHub(request) {
    const given = request.headers.get("Authorization") ?? "";
    if (!this.env.HUB_KEY) return false;
    const [a, b] = await Promise.all([
      digest(given),
      digest(`Bearer ${this.env.HUB_KEY}`),
    ]);
    return crypto.subtle.timingSafeEqual(a, b);
  }

  /**
   * The fingerprint of the paired device that signed this request, or null: its certificate,
   * the time, and its Ed25519 signature over "herder-relay\n<host>\n<time>".
   */
  async device(request) {
    try {
      const headers = request.headers;
      const cert = base64(headers.get("x-herder-device"));
      const time = headers.get("x-herder-time") ?? "";
      const signature = base64(headers.get("x-herder-signature"));
      if (!/^\d{1,12}$/.test(time) || Math.abs(Date.now() / 1000 - Number(time)) > SKEW) {
        return null;
      }
      const fingerprint = hex(await crypto.subtle.digest("SHA-256", cert));
      if (!(await this.paired()).has(fingerprint)) return null;
      const at = find(cert, ED25519_SPKI);
      if (at < 0 || at + ED25519_SPKI.length + 32 > cert.length) return null;
      const spki = cert.slice(at, at + ED25519_SPKI.length + 32);
      const key = await crypto.subtle.importKey("spki", spki, { name: "Ed25519" }, false, [
        "verify",
      ]);
      const signed = new TextEncoder().encode(
        `herder-relay\n${new URL(request.url).host}\n${time}`,
      );
      const ok = await crypto.subtle.verify({ name: "Ed25519" }, key, signature, signed);
      return ok ? fingerprint : null;
    } catch {
      return null;
    }
  }
}

/** Which end of which stream a socket is, from its tags: `["c" | "h", id]`, or `[]`. */
function stream(tags) {
  const tag = tags.find((tag) => tag.startsWith("c:") || tag.startsWith("h:"));
  return tag ? [tag[0], tag.slice(2)] : [];
}

function refuse(status, message) {
  return new Response(message, { status });
}

/** Closes `socket`, which may be closed already. */
function close(socket, code, reason) {
  try {
    socket.close(code, reason.slice(0, 120));
  } catch {
    // Closed already.
  }
}

/** A close code a socket may send: not one only the runtime reports. */
function sendable(code) {
  const reserved = [1004, 1005, 1006, 1015];
  return (code >= 1000 && code < 1015 && !reserved.includes(code)) || (code >= 3000 && code < 5000)
    ? code
    : 1000;
}

async function digest(text) {
  return crypto.subtle.digest("SHA-256", new TextEncoder().encode(text));
}

function base64(text) {
  if (!text) throw new Error("missing");
  return Uint8Array.from(atob(text), (c) => c.charCodeAt(0));
}

function hex(bytes) {
  return [...new Uint8Array(bytes)].map((b) => b.toString(16).padStart(2, "0")).join("");
}

function find(haystack, needle) {
  outer: for (let i = 0; i + needle.length <= haystack.length; i++) {
    for (let j = 0; j < needle.length; j++) {
      if (haystack[i + j] !== needle[j]) continue outer;
    }
    return i;
  }
  return -1;
}
