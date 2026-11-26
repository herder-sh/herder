# Public access through a Cloudflare Tunnel

Goal: reach a machine at a public hostname, such as `wss://box.example.com`, from a phone
that is on neither the machine's LAN nor Tailscale, with the same security as a direct
connection.

Basecamp: P23 · Public access through a Cloudflare Tunnel.

To expose only the vault and reach every machine through it, see
[vault-relay.md](vault-relay.md) (P24).

## How a connection works today

A client dials `host:port` (7447 by default) over TCP and runs, in order
(`crates/herder-daemon/src/ws/conn.rs`):

1. **TLS 1.3, mutual.** The daemon presents its self-signed certificate
   (`ws/tls.rs`); the client pins its SHA-256 fingerprint, which it got from the
   link or invite it added the machine with (`herder-client-core/src/auth.rs`). The client
   presents its device certificate; TLS 1.3 makes it sign the transcript, so each
   connection proves it holds the device key. The daemon knows the device by that
   certificate's fingerprint.
2. **WebSocket upgrade.**
3. **Hello, within 10 s.** A paired device is authenticated by its fingerprint. An unpaired
   device can only sign in: with a passkey assertion, an invite (100 bits, 24 h), a claim
   code (30 bits, 10 min, opened by `herder install` / `herder claim` on the machine), or a
   login another device of the person approves. Anything else is refused and the connection
   closes. **No command is read before the server hello.**

Each daemon serves only this endpoint. It has no other HTTP routes, no unauthenticated API
and no web UI.

## Why a plain tunnel would break this

Cloudflare terminates TLS at its edge. Through `cloudflared` with an `https://` origin:

- the client sees Cloudflare's certificate, so pinning the daemon's fingerprint fails, and
  anything that doesn't pin would trust Cloudflare as a man in the middle;
- Cloudflare doesn't forward a client certificate it did not issue, so the daemon loses
  the device identity that all of its auth rests on.

Rebuilding device auth as a signed challenge in the protocol would change
`herder-protocol` and still leave session contents readable at Cloudflare's edge.

## Design: the daemon connection, inside a WebSocket

```
phone ──wss (Cloudflare cert)──▶ Cloudflare edge ──tunnel──▶ cloudflared ──ws──▶ daemon tunnel listener
      └──────────── TLS 1.3 mTLS (pinned daemon cert + device cert) + WebSocket + hello ─────────────┘
```

- **Outer layer.** The client opens `wss://box.example.com/`, verifying Cloudflare's
  certificate against the public web PKI (bundled Mozilla roots, `webpki-roots`).
  `cloudflared` forwards the upgrade to the daemon's **tunnel listener**, a plain-HTTP
  WebSocket endpoint on loopback.
- **Inner layer.** The bytes of the WebSocket's binary frames are exactly a direct
  connection's TCP bytes: the same TLS 1.3 handshake with the pinned daemon certificate and
  the device certificate, then the inner WebSocket, hello and authentication. The daemon
  runs the same `serve` path for both.

Result: Cloudflare relays only ciphertext. Device identity, pinning, pairing, passkeys,
roles and owner-only terminals behave exactly as they do on a direct connection.
`herder-protocol`'s message types are untouched. The outer layer costs one extra TLS
session and WebSocket framing.

### Daemon

```toml
# ~/.config/herder/config.toml
listen = "127.0.0.1:7447"        # optional: no direct listener on the network at all
tunnel = "127.0.0.1:7448"        # where cloudflared delivers; must be loopback
```

The tunnel listener:

- refuses to start unless `tunnel` is a loopback address, since only `cloudflared` on the
  same machine may reach it (a vault's may be any address, for a `cloudflared` in another
  pod; see [vault-relay.md](vault-relay.md));
- accepts only WebSocket upgrades and answers any other request with an error status. The
  path is ignored, because the hostname already picks the machine;
- refuses any request with an `Origin` header (403). Browsers always send one, and herder
  clients never do, so a web page can't open a cross-site WebSocket to it. The inner TLS
  would stop one anyway, because a browser has no device key;
- gives the outer handshake, the inner TLS handshake and the hello the same 10 s limit as
  a direct connection.

### cloudflared

```yaml
# ~/.cloudflared/config.yml
tunnel: <tunnel-id>
credentials-file: /home/me/.cloudflared/<tunnel-id>.json
ingress:
  - hostname: box.example.com
    service: http://127.0.0.1:7448
  - service: http_status:404
```

```sh
cloudflared tunnel create box
cloudflared tunnel route dns box box.example.com
cloudflared tunnel run box          # or: cloudflared service install
```

Cloudflare proxies WebSockets on every plan, and `http://` is correct here: privacy comes
from the inner TLS, and the hop runs on loopback.

### Clients

A machine address is `host[:port]` as before, or a URL: `wss://host[:port][/path]` (or
`ws://` for a reverse proxy on a trusted network; the inner TLS protects the session either
way). `herder-client-core` normalizes and saves addresses (`address.rs`) and races them
(`supervisor.rs`):

- for a URL it dials the host, runs the outer TLS and WebSocket upgrade, then the inner TLS
  with the pinned fingerprint and device certificate over the frames, then the inner
  WebSocket;
- tunnel URLs rank last by default (LAN, other, Tailscale, tunnel), since they hop through
  Cloudflare. The user can still reorder them;
- sharing a fleet link includes tunnel URLs, which aren't loopback.

The TUI and both Apple apps get this through client-core (the apps use it through
`herder-ffi`; HerderKit has no networking of its own). Adding the URL to a machine works
today through `Client::set_addresses`, which is the machine page's address list.

## Is it safe on the public internet?

| Requirement | Status |
| --- | --- |
| Every connection authenticates before any command | Yes, today: the device certificate is required in the TLS handshake, the hello comes within 10 s, and authenticate or sign-in runs before the server hello; a refusal closes the connection (`conn.rs` `read`). Through the tunnel this is unchanged, because it is the same code path inside the frames. |
| No unauthenticated endpoints | The tunnel listener serves only the upgrade. Pre-auth, a stranger can only ask for a sign-in challenge. |
| Server authenticity end to end | The daemon fingerprint is pinned through the inner TLS. Cloudflare can't impersonate the daemon. |
| Session confidentiality from Cloudflare | Inner TLS 1.3. Cloudflare sees sizes and timing only. |
| Origin checks | Requests with `Origin` get 403 on the tunnel listener. |
| Brute force of codes | Invites (100 bits) and host pairing codes (50 bits) are out of reach. **Gap:** the claim code has 30 bits, an open window allows unlimited guesses, and a correct guess plus any passkey makes the guesser the owner. Fix: the window closes after 5 wrong codes (P23.2). |
| Rate limiting | Every tunnel peer is `127.0.0.1`, so per-peer limits see one peer. Cap connections that are not yet authenticated, per `CF-Connecting-IP` (trusted only on the loopback tunnel listener) and in total (P23.5). Cloudflare's own DDoS protection sits in front. |
| Login-approval spam | An unpaired device can ask a person's apps to approve it. Each request shows a code that must match by eye, and P23.5's cap bounds the number. |
| Provider login tokens | Untouched. Nothing here reads, stores or relays them. |
| Terminals owner-only | Untouched. Roles come from the same auth. |

### Cloudflare Access (optional, never required)

An owner can also put the hostname behind Cloudflare Access with a service-token policy.
The client would then send `CF-Access-Client-Id` / `CF-Access-Client-Secret` on the outer
upgrade only, never to the daemon. That needs a per-address secret in the client profile
and a client-core API to set it, so it is a contract change (P23.7). Without it, leave the
hostname outside Access (or bypass it): the inner mutual TLS already authenticates every
connection.

## Contracts

| Change | Contract? |
| --- | --- |
| Daemon `tunnel` config key and listener (P23.3) | No: config and daemon internals. |
| Client-core accepting URL addresses and connecting through them (P23.4) | No signature changes. `set_addresses` / link parsing accept more strings. The saved profile keeps `addresses: Vec<String>`. |
| Claim attempt limit (P23.2), pre-auth cap (P23.5) | No. |
| Daemon advertising its public URL in `ServerHello.addresses`, invites, `OwnershipClaim` (P23.6) | **Yes**: herder-protocol documents those `addresses` as `host:port`. Older clients would reject a link carrying a URL. Until it lands, add the URL from the machine page after the first connection. |
| Cloudflare Access service tokens (P23.7) | **Yes**: new client-core API and profile field. |

## Todos

- **P23.1 · Public access design** (this doc).
- **P23.2 · A claim window closes after repeated wrong codes.** Done when the fifth wrong
  code closes the window and the right code is then refused, tested.
- **P23.3 · Daemon tunnel listener for cloudflared.** Done when a client connects through
  the tunnel listener with its device certificate and is authenticated exactly as on a
  direct connection, an unpaired device is refused, an `Origin` request gets 403, and a
  non-loopback `tunnel` fails config load, all tested.
- **P23.4 · Clients connect through wss:// machine addresses.** Done when a client with a
  tunnel address streams a fake-adapter session through an in-process daemon's tunnel
  listener, a wrong pinned fingerprint is still refused through it, and normalization and
  ordering tests cover URLs.
- **P23.5 · Limit unauthenticated connections on the tunnel.** Done when connections past
  the cap are refused before TLS, authenticated ones aren't counted, and tests cover both.
- **P23.6 · [CONTRACT] Daemons advertise their public URL.** Done when protocol docs and
  schema allow URLs in `addresses`, invites and claim links carry the public URL, and an
  app adds a machine from such a link through the tunnel only.
- **P23.7 · [CONTRACT] Optional Cloudflare Access service token per machine.** Done when a
  machine with a token connects through an Access-protected hostname, one without gets a
  clear error, and the secret never reaches the daemon.
- **P23.8 · Apple apps: add a machine's public URL.** Done when an iOS device connects to
  a machine reachable only through its tunnel URL, and a UI test adds the URL.
