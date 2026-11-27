# A relay in your own Cloudflare account

Goal: reach the hub, and through it every machine, from anywhere, for someone with no public
IP, no router to forward a port on and no domain. herder deploys a small Cloudflare Worker
with a Durable Object to the owner's own Cloudflare account, on `workers.dev`; the hub and
the devices both connect out to it, and it pairs their connections.

Basecamp: P27.5 · Durable Object relay in the user's Cloudflare account. Part of P27 ·
Remote access options, which ranks it among the other routes. Builds on [public-access.md](public-access.md) and [hub-relay.md](hub-relay.md).

## Set it up

On the hub's machine (in a cluster, `kubectl exec` into the hub):

```sh
herder hub relay-worker deploy      # asks for a Cloudflare API token once
herder hub relay-worker status
herder hub relay-worker remove
```

Create the token at dash.cloudflare.com → My Profile → API Tokens → Create Token → Custom,
with the account permissions **Workers Scripts: Edit** and **Account Settings: Read**. The
token may also come from `CLOUDFLARE_API_TOKEN`. If it reaches several accounts, name one
with `--account <id>`. An account that never opened Workers & Pages has no `workers.dev`
subdomain yet: open it once in the dashboard to pick one.

The hub keeps the token, the account, the relay's URL and its own key to the relay in
`<data_dir>/relay-worker.json`, readable by its user only. It never sends the token or the
key to a device. It deploys the Worker again when its bundled source changes, so updating
herder updates the relay. `remove` deletes the script and its Durable Object's storage.

The relay runs at `https://herder-relay.<subdomain>.workers.dev`.

## Design

```
app ──wss (Cloudflare cert), signed upgrade──▶ Worker ─┐
                                                       ├─ Durable Object pairs the two sockets
hub ──wss, hub key: /hub (one, kept open) ─────▶ Worker │  and copies binary frames
hub ──wss, hub key: /accept/<id> (per stream) ─▶ Worker ┘
 └──────────── TLS 1.3 (pinned hub cert + device cert) + WebSocket + hello, end to end ────────┘
```

- **The hub keeps one connection open** to the relay (`/hub`), authenticated with the
  hub key: 32 random bytes the hub made when it deployed the relay, stored as the script's
  secret binding `HUB_KEY`. On it the hub tells the relay the SHA-256 certificate
  fingerprints of every device paired with it (`{"devices": [...]}`), when it connects and
  within 5 s of any pairing or revocation. The relay keeps the list in its storage.
- **A device connects** to the relay's URL with three headers on its upgrade:
  `x-herder-device` (its device certificate, base64 DER), `x-herder-time` (unix seconds) and
  `x-herder-signature` (Ed25519, with its device key, over
  `herder-relay\n<host>\n<time>`, where host is the URL's `host[:port]`).
- **The relay tells the hub** (`{"open": "<id>", "client": "<ip>"}`), and the hub opens a
  second connection for the stream (`/accept/<id>`, with the hub key). The relay holds the
  device's first frames (its TLS's first flight) until then, at most 10 s.
- **The relay copies frames.** Each binary frame of one socket goes to the other; a text
  frame closes the stream. Closing one end closes the other.
- **The hub serves the stream as its tunnel listener's**: the frames carry the TLS 1.3 of a
  direct connection with the pinned hub certificate and the device certificate, then the
  WebSocket and hello. The same gate caps connections not yet signed in, per the client
  address the relay names. Paths inside work as on any connection to the hub, so
  `/machines/<host>` reaches a machine through it ([hub-relay.md](hub-relay.md)).

Hosts reach their hub through the relay the same way, signing with their own device key.

### Status

Done: the Worker, deploying, updating and removing it, and the hub's side, tested end to end
against the Worker in a local workerd. Next, once P27.1 fixes the route model: devices and
hosts dial the relay as a route of the hub, ranked as P27.1 says, and a session streams
from a machine through it in a test.

### Who gets in

| Who | Proof | Refusal |
| --- | --- | --- |
| The hub, on `/hub` and `/accept/<id>` | `Authorization: Bearer <hub key>`, compared in constant time | 401 |
| A device | Its certificate's fingerprint is in the hub's list; the Ed25519 signature checks with the certificate's key; the time is within 5 minutes | 401 |
| A browser | Any `Origin` header | 403 |
| Anyone, while the hub is not connected | | 503 |
| A request that is not a WebSocket upgrade | | 426 |

A device the hub unpairs loses its open streams through the relay at once and gets no new
one. A stranger never reaches the hub at all, so it costs the hub nothing.

What the relay cannot do: read or alter anything. It has neither the hub's TLS key nor a
device's. A captured signed upgrade could be replayed for 5 minutes by whoever sees inside
Cloudflare's TLS, that is Cloudflare, and it would still fail the device certificate inside.

A device that is not paired yet cannot sign in through the relay; it signs in at another
address first, as behind a front door that wants a header ([public-access.md](public-access.md)).

### Cost

The relay uses the WebSocket Hibernation API: between frames the Durable Object is evicted
from memory and billed nothing, and Cloudflare answers the hub's pings without waking it.
Cloudflare's published pricing (2026):

| | Workers Free | Workers Paid ($5/month) |
| --- | --- | --- |
| Requests (each upgrade; incoming WebSocket messages count 1 in 20) | 100,000 a day | 10 million a month included |
| Durable Object duration (while awake, 128 MB) | 13,000 GB-s a day: about 28 hours awake a day | 400,000 GB-s a month included |
| SQLite storage (the device list: a few KB) | 5 GB | 5 GB included |

One person with a phone and a laptop, a few hours a day, sends well under 2 million
messages a day (100,000 requests) and keeps the object awake for a few hours: **free**.
Only a household streaming many busy sessions all day would need Workers Paid, $5 a month.
Outgoing messages and bandwidth are not billed.

### Why not the hub relay alone

The hub relay ([hub-relay.md](hub-relay.md)) still needs the hub reachable: a port forward,
a VPN, or a tunnel with a domain. The relay Worker needs none of them; it is the last
route, for when nothing else works.

## Tests

- `deploy/cloudflare-relay/test/` runs the Worker in a local workerd (miniflare): unauthenticated
  and wrongly signed devices, the wrong hub key, browsers and a missing hub are refused;
  frames are held, then forwarded both ways; closing and unpairing close streams.
- `crates/herder-daemon/src/hub/cloudflare.rs` deploys, updates and removes against a mocked
  Cloudflare API.
- `crates/herder-daemon/src/hub/relay_worker.rs` runs a hub against the Worker in miniflare:
  a paired device reaches the hub through it; a stranger, a device signing with another's
  certificate, and a device once revoked are refused by the Worker.

The Worker's tests need Node 22 and `npm ci` in `deploy/cloudflare-relay`; CI runs them in the
`ci` job.
