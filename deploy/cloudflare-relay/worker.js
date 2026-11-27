// @ts-check
// herder's Durable Object relay: a Cloudflare Worker that herder's hub deploys to its owner's
// own Cloudflare account, so devices reach machines that have no public address
// (docs/remote-access.md, "Durable Object relay").
//
// Each machine (each host linked to the hub, and the hub itself) keeps one WebSocket open to
// the relay at /hosts/<host id>. A device opens /machines/<host id> and starts its TLS right
// behind the upgrade; the machine's Durable Object tells the machine {"open": "<stream id>",
// "device": "<the token's sub>", "client": "<the device's IP>"},
// the machine opens /streams/<stream id>, and the Durable Object copies every binary frame
// from each of the two sockets to the other. Inside, the device and the machine run herder's
// mutual TLS, so the relay forwards ciphertext only.
//
// Every upgrade carries `Authorization: Bearer <token>`, a token the hub signed with the
// secret it set as RELAY_SECRET when it deployed the relay. The Worker checks it before any
// Durable Object is touched: a stranger costs one Worker request and no more.
//
// Sockets are accepted through the hibernation API: between frames the Durable Object is out
// of memory and billed nothing, and an idle machine's connection costs nothing.

import { DurableObject } from "cloudflare:workers";

/** How long a device's stream waits for its machine to open its end, in milliseconds. */
const MACHINE_WAIT = 10_000;

/** Frames a device may send before its machine opened its end: its TLS's first flight. */
const MAX_HELD = 64;

/** A host id, a stream id: what the paths name. */
const ID = /^[A-Za-z0-9._-]{1,128}$/;

/**
 * @typedef {{ sub: string, kind: "client" | "host", exp: number }} Claims
 * @typedef {{ RELAY: DurableObjectNamespace, RELAY_SECRET: string }} Env
 */

export default {
  /**
   * @param {Request} request
   * @param {Env} env
   */
  async fetch(request, env) {
    if (request.headers.get("Upgrade")?.toLowerCase() !== "websocket") {
      return refuse(426, "herder's relay takes WebSocket upgrades only");
    }
    if (request.headers.has("Origin")) {
      return refuse(403, "browsers may not connect");
    }
    const claims = await verify(request.headers.get("Authorization"), env.RELAY_SECRET);
    if (!claims) {
      return refuse(401, "no valid relay token; the hub issues one");
    }
    const [, route, id, ...rest] = new URL(request.url).pathname.split("/");
    if (rest.length || !id || !ID.test(id)) return refuse(404, "no such path");
    // The machine whose Durable Object the request goes to.
    let machine;
    if (route === "machines" && claims.kind === "client") {
      machine = id;
    } else if (route === "hosts" && claims.kind === "host" && claims.sub === id) {
      machine = id;
    } else if (route === "streams" && claims.kind === "host") {
      machine = claims.sub;
    } else if (route === "machines" || route === "hosts" || route === "streams") {
      return refuse(401, "this relay token is not for this path");
    } else {
      return refuse(404, "no such path");
    }
    // The Durable Object trusts what the Worker verified: who the token names.
    const forwarded = new Request(request);
    forwarded.headers.set("x-herder-sub", claims.sub);
    return env.RELAY.get(env.RELAY.idFromName(machine)).fetch(forwarded);
  },
};

/**
 * The claims of `authorization` (`Bearer v1.<claims>.<mac>`) if `secret` signed it and it has
 * not expired.
 *
 * @param {string | null} authorization
 * @param {string} secret
 * @returns {Promise<Claims | null>}
 */
export async function verify(authorization, secret) {
  try {
    if (!secret || !authorization?.startsWith("Bearer ")) return null;
    const token = authorization.slice("Bearer ".length).trim();
    const [version, claims, mac, ...rest] = token.split(".");
    if (version !== "v1" || !claims || !mac || rest.length) return null;
    const key = await crypto.subtle.importKey(
      "raw",
      new TextEncoder().encode(secret),
      { name: "HMAC", hash: "SHA-256" },
      false,
      ["verify"],
    );
    const signed = new TextEncoder().encode(`v1.${claims}`);
    if (!(await crypto.subtle.verify("HMAC", key, base64url(mac), signed))) return null;
    /** @type {Claims} */
    const parsed = JSON.parse(new TextDecoder().decode(base64url(claims)));
    if (typeof parsed.sub !== "string" || !ID.test(parsed.sub)) return null;
    if (parsed.kind !== "client" && parsed.kind !== "host") return null;
    if (typeof parsed.exp !== "number" || parsed.exp <= Date.now() / 1000) return null;
    return parsed;
  } catch {
    return null;
  }
}

/** One machine's meeting point: its connection, and the streams to it. */
export class Relay extends DurableObject {
  /**
   * @param {DurableObjectState} ctx
   * @param {Env} env
   */
  constructor(ctx, env) {
    super(ctx, env);
    /** @type {Map<string, { frames: ArrayBuffer[], timer: ReturnType<typeof setTimeout> }>} */
    this.waiting = new Map();
  }

  /** @param {Request} request */
  async fetch(request) {
    const [, route, id] = new URL(request.url).pathname.split("/");
    if (route === "hosts") {
      for (const old of this.ctx.getWebSockets("machine")) {
        close(old, 1000, "the machine connected again");
      }
      return this.accept(["machine"], () => {});
    }
    if (route === "streams") {
      const held = this.waiting.get(id);
      if (!held) return refuse(404, "no device waits for this stream");
      this.waiting.delete(id);
      clearTimeout(held.timer);
      return this.accept([`m:${id}`], (socket) => {
        for (const frame of held.frames) socket.send(frame);
      });
    }
    const [machine] = this.ctx.getWebSockets("machine");
    if (!machine) return refuse(503, "the machine is not connected to this relay");
    const stream = hex(crypto.getRandomValues(new Uint8Array(16)));
    return this.accept([`d:${stream}`], (socket) => {
      const timer = setTimeout(() => {
        this.waiting.delete(stream);
        this.gone(socket, 1011, "the machine did not open its end");
      }, MACHINE_WAIT);
      this.waiting.set(stream, { frames: [], timer });
      machine.send(
        JSON.stringify({
          open: stream,
          // The device as the hub knows it, and its address as Cloudflare saw it.
          device: request.headers.get("x-herder-sub"),
          client: request.headers.get("CF-Connecting-IP"),
        }),
      );
    });
  }

  /**
   * Answers the upgrade with a socket tagged `tags`, after `then` ran on it.
   *
   * @param {string[]} tags
   * @param {(socket: WebSocket) => void} then
   */
  accept(tags, then) {
    const [client, server] = Object.values(new WebSocketPair());
    this.ctx.acceptWebSocket(server, tags);
    then(server);
    return new Response(null, { status: 101, webSocket: client });
  }

  /**
   * @param {WebSocket} socket
   * @param {string | ArrayBuffer} message
   */
  async webSocketMessage(socket, message) {
    const ends = stream(this.ctx.getTags(socket));
    if (!ends) return; // The machine's connection says nothing the relay acts on.
    const { end, id } = ends;
    if (typeof message === "string") {
      return this.gone(socket, 1003, "the relay carries binary frames only");
    }
    if (end === "d") {
      const [peer] = this.ctx.getWebSockets(`m:${id}`);
      if (peer) return peer.send(message);
      const held = this.waiting.get(id);
      if (held && held.frames.length < MAX_HELD) return void held.frames.push(message);
      this.gone(socket, 1011, "the machine did not open its end");
    } else {
      const [peer] = this.ctx.getWebSockets(`d:${id}`);
      if (peer) peer.send(message);
      else this.gone(socket, 1000, "the device is gone");
    }
  }

  /**
   * @param {WebSocket} socket
   * @param {number} code
   * @param {string} reason
   */
  webSocketClose(socket, code, reason) {
    this.gone(socket, code, reason);
  }

  /** @param {WebSocket} socket */
  webSocketError(socket) {
    this.gone(socket, 1011, "the other end broke");
  }

  /**
   * Closes `socket` and, if it is an end of a stream, the stream's other end.
   *
   * @param {WebSocket} socket
   * @param {number} code
   * @param {string} reason
   */
  gone(socket, code, reason) {
    close(socket, sendable(code), reason);
    const ends = stream(this.ctx.getTags(socket));
    if (!ends) return;
    const { end, id } = ends;
    for (const other of this.ctx.getWebSockets(`${end === "d" ? "m" : "d"}:${id}`)) {
      close(other, sendable(code), reason);
    }
    const held = this.waiting.get(id);
    if (held) {
      clearTimeout(held.timer);
      this.waiting.delete(id);
    }
  }
}

/**
 * Which end of which stream a socket is, from its tags: the device's (`d`) or the machine's
 * (`m`), and the stream id; or null for the machine's connection.
 *
 * @param {string[]} tags
 * @returns {{ end: "d" | "m", id: string } | null}
 */
function stream(tags) {
  const tag = tags.find((tag) => tag.startsWith("d:") || tag.startsWith("m:"));
  return tag ? { end: tag[0] === "d" ? "d" : "m", id: tag.slice(2) } : null;
}

/**
 * @param {number} status
 * @param {string} message
 */
function refuse(status, message) {
  return new Response(message, { status });
}

/**
 * Closes `socket`, which may be closed already.
 *
 * @param {WebSocket} socket
 * @param {number} code
 * @param {string} reason
 */
function close(socket, code, reason) {
  try {
    socket.close(code, reason.slice(0, 120));
  } catch {
    // Closed already.
  }
}

/**
 * A close code a socket may send: not one only the runtime reports.
 *
 * @param {number} code
 */
function sendable(code) {
  const reserved = [1004, 1005, 1006, 1015];
  return (code >= 1000 && code < 1015 && !reserved.includes(code)) || (code >= 3000 && code < 5000)
    ? code
    : 1000;
}

/** @param {string} text */
function base64url(text) {
  const base64 = text.replace(/-/g, "+").replace(/_/g, "/");
  return Uint8Array.from(atob(base64 + "=".repeat((4 - (base64.length % 4)) % 4)), (c) =>
    c.charCodeAt(0),
  );
}

/** @param {Uint8Array} bytes */
function hex(bytes) {
  return [...bytes].map((b) => b.toString(16).padStart(2, "0")).join("");
}
