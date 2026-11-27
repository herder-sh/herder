# Reaching your machines from anywhere

Goal: from a phone or laptop on any network, reach every machine with the same security as
at home, through whichever of several routes the user has, chosen per user, without
fighting the device's other VPNs, and without draining the phone.

Basecamp: P27 · Remote access options. Builds on [public-access.md](public-access.md)
(P23, P26: public addresses, proxy headers, the reach check) and
[hub-relay.md](hub-relay.md) (P24, P26.8: the hub forwards a stream to any machine by path).
Supersedes P26.9 (QUIC, dropped) and parks iroh ("Later · Revisit iroh").

## The routes

A machine is reached by one of these, best first:

| Rank | Route | Address form | What carries it | Set up |
| --- | --- | --- | --- | --- |
| 0 | **Home network** | `192.168.1.10:7447` | The LAN | Nothing |
| 1 | **Own VPN** | `wg://<tunnel>/192.168.1.10:7447` | herder's built-in WireGuard client, to the user's WireGuard server (e.g. a UniFi router) | Import the server's config once (QR or `.conf`) |
| 2 | Direct, public | `home.example.net:7447` | A port forward or TCP proxy | A public address (P26.1) |
| 3 | **System VPN** | `100.101.102.103:7447`, `box.tail1234.ts.net:7447` | Tailscale, ZeroTier, a VPN the OS runs | Nothing in herder |
| 4 | **Hub via herder's WireGuard** | `wg://hub/10.247.0.1:7447` (on the hub only) | herder's built-in WireGuard client, to the hub's own WireGuard endpoint | One toggle; one UDP port forwarded to the hub |
| 5 | Tunnel URL | `wss://hub.example.com` | Cloudflare Tunnel, hand-configured (P23) or managed by herder (P27.6) | A tunnel; managed: a Cloudflare token |
| 6 | Through the hub | `hub://<hub host id>` (on other machines) | The hub's relay (P24), the hub reached by its own best route | Nothing |
| 7 | **Durable Object relay** | `relay+wss://herder-relay.<account>.workers.dev` | A Worker and Durable Object in the user's Cloudflare account (P27.5) | A Cloudflare token |

The ranks are the order `herder-client-core` sorts a machine's addresses into
(`address::rank`): home network > own VPN > system VPN > hub via herder's WireGuard > hub
via a tunnel URL or the Durable Object relay. Routes through the hub compose: a machine's
`hub://` address dials the hub at the hub's own addresses, in the hub's order, so "a machine
through the hub, the hub through herder's WireGuard" is `hub://` on the machine and
`wg://hub/…` on the hub, and ranks above "through the hub, the hub through its tunnel URL"
without a route kind of its own.

Whatever the route, the connection inside it is the same: TLS 1.3 pinning the machine's
certificate and presenting this device's certificate, the WebSocket, the hello
([public-access.md](public-access.md), "How a connection works today"). Every carrier below
sees ciphertext, and every machine authenticates and authorizes exactly as on its LAN.

### Own VPN: herder's built-in WireGuard client (P27.3)

For a user who already runs a WireGuard server, typically on their router (UniFi, OPNsense,
a Fritz!Box, `wg-quick` on a Pi). herder carries **only its own connections** through it,
in userspace: no system VPN profile, no TUN device, no route table change. A laptop that
must keep a corporate Cloudflare WARP (or any other VPN) on keeps it, and herder still
reaches home.

- **Configured**: the user exports a peer config on the router and imports it once into
  herder, as a QR code (the apps' camera) or the `.conf` text (file, paste, or
  `herder vpn import <file>` in the TUI's CLI). It is a standard `wg-quick` config:
  `[Interface] PrivateKey, Address` (`DNS`, `MTU` read, `DNS` ignored) and one `[Peer]
  PublicKey, PresharedKey?, Endpoint, AllowedIPs` (`PersistentKeepalive` ignored: herder's
  own pings keep the path warm while it is used, and nothing should while it is not).
- **Stored**: in `<config_dir>/wireguard.json`, mode 0600 in the 0700 config dir, beside the
  device keys in `machines.json`, and only there: the private key never leaves the device,
  is never sent to a daemon, and never goes in a fleet link. Removing the tunnel deletes it.
- **Used**: client-core adds `wg://<tunnel>/<address>` to a machine, right after its home
  network addresses, for each of the machine's home-network (rank 0) addresses inside the
  tunnel's `AllowedIPs`. At home the plain address answers first; away it fails fast (no
  route, or no answer within its head start) and the tunnel's answers. A config with
  `AllowedIPs = 0.0.0.0/0` covers every home address, which is what a router's default
  export says.
- **Carried**: a userspace WireGuard device (`boringtun`'s `Tunn`, see Crates) on one UDP
  socket, and a userspace TCP/IP stack (`smoltcp`) whose TCP sockets client-core's dial
  uses instead of a kernel `TcpStream`. The rest of the dial (TLS, upgrade, hello) is
  unchanged.

### Hub via herder's WireGuard (P27.4)

For a user with a hub at home and no VPN of their own: the hub runs a userspace WireGuard
endpoint on one UDP port, and devices enable it with one toggle.

- **Hub side**: `GetRemoteAccess`/`SetRemoteAccess` (owners) turn on
  `RemoteAccessSettings::wireguard`: the UDP `port` it binds (default 51820) and the
  `endpoint` devices send to (`home.example.net:51820`, the router's public name and the
  forwarded port). The hub generates its WireGuard key once, in its data dir. No pod
  privileges: a UDP socket, no TUN, no `NET_ADMIN`. The router forwards that one UDP port
  to the hub (`deploy/hub.yaml` gets it on the Service).
- **Enrolled through pairing**: the device's toggle has client-core generate an X25519 key
  on the device and send its public key with `EnrollWireGuard` on its ordinary,
  authenticated connection to the hub (at home, or through the tunnel URL). The hub stores
  that key on the paired device's record (`auth.json`, beside its certificate fingerprint),
  gives it a tunnel address from `10.247.0.0/16`, and answers `WireGuardEnrolled` with the
  hub's public key, the endpoint and the addresses. The device saves the enrollment with that
  machine in `machines.json` (private key included, never sent anywhere) and adds
  `wg://hub/<hub address>` to the hub's addresses at rank 4. Off: `UnenrollWireGuard`, and
  the local key and address are deleted.
- **Revoked with the device**: the key lives on the device's record, so `RevokeDevice`,
  `RevokePerson` or a re-enrollment drops the peer at once, and its session with it.
- **Silent to strangers**: the endpoint answers only a handshake initiation from an
  enrolled key. A packet without a valid MAC1 (which takes the hub's public key, given only
  to enrolled devices) is dropped unanswered, as is a valid initiation from a key not
  enrolled. A port scan sees a closed UDP port.
- **Inside the tunnel**: only TCP to `10.247.0.1` on the hub's herder port (its `listen`
  port) is accepted; everything else is dropped. The hub's userspace stack hands each
  accepted connection to the same `serve` path a TCP socket gets, so the inner herder TLS
  and device authentication run as on the LAN, and `hub://` routes to every machine work
  through it unchanged: the hub never forwards IP packets to the LAN.

### System VPN

Tailscale, ZeroTier, a corporate or OS VPN: they already work, since the machine lists
their addresses and the OS routes them. herder recognizes Tailscale's addresses for ranking
(`100.64.0.0/10`, `fd7a:115c:a1e0::/48`, `*.ts.net`).

### Hub via a tunnel URL, hand-made or managed (P27.6)

The hub's `wss://` public address (P23, P26.1). P27.6 lets the hub create and run it
itself through the Cloudflare API instead of a hand-configured `cloudflared`:
`RemoteAccessSettings::cloudflare` with `tunnel_hostname` set. The hub runs a pinned
`cloudflared` with the tunnel's token, publishes the hostname as one of its public
addresses (so devices follow it with nothing to type), and reports the tunnel's state in
`RemoteAccess::tunnel`. An optional Cloudflare Access in front stays a proxy header (P26.2,
P27.7).

### Durable Object relay (P27.5)

For a user without a public IP or control of their router, and without a domain: a small
Worker and Durable Object, source in the repo, that the hub deploys to the user's own
Cloudflare account through the API (`RemoteAccessSettings::cloudflare` with `relay`), on
`workers.dev`.

- **Who connects**: hosts linked to the hub and the hub itself keep one WebSocket out to
  the Worker each, at `/hosts/<host id>` (control). A client connects out to
  `/machines/<host id>` and starts its inner TLS right behind the upgrade, as on the hub's
  relay. The Durable Object for that host tells the host's control socket
  `{"open":"<stream id>","device":"<the token's sub>","client":"<the device's IP>"}`, the
  host dials `/streams/<stream id>`, and the Durable Object
  pairs the two sockets and copies binary frames between them. It is the hub relay's shape
  (P24) on Cloudflare's edge, so the host serves the stream with the same code.
- **Authenticated before it forwards**: every upgrade carries `Authorization: Bearer
  <token>`, a [`RelayGrant`] the hub issued: `v1.<claims>.<mac>`, where `claims` is
  base64url JSON `{"sub": "<device or host id>", "kind": "client" | "host", "exp": <unix
  seconds>}` and `mac` is base64url HMAC-SHA256 over `v1.<claims>` with a secret the hub
  generated and set as the Worker's secret binding at deploy. The Worker refuses (401,
  before any Durable Object is touched) a missing, malformed, expired or wrongly signed
  token, a `client` token on `/hosts/` or `/streams/`, and a `host` token whose `sub` is not
  the path's host. Client tokens last 24 hours and client-core renews one (`GetRelayToken`)
  whenever it is connected to the hub and its token is over half used; host tokens last 7
  days and the hub re-sends them on the replication connection (`HubMessage::RelayGrant`).
  A revoked device cannot renew, and is refused by every machine inside anyway.
- **Forwards ciphertext only**: the Worker never sees a key of the inner TLS. It holds the
  HMAC secret only, which lets it decide who may knock, never who gets in.
- **Address**: `relay+wss://<worker host>` (`relay+ws://` for a local `workerd` in tests),
  added by client-core, rank 7, to every machine the hub lists as relayed and to the hub
  itself, once the hub's `RemoteAccess::relay` is up.

The Cloudflare API token is entered once, as a hub secret (`PutSecret`, write-only), and
named by `CloudflareSettings::api_token`; it never goes back to a client, and machines never
get it. It needs the account permissions **Workers Scripts: Edit** (and **Account Settings:
Read**, to find the account's `workers.dev` subdomain). An account that never opened
Workers & Pages has no subdomain yet; the relay's status says to open it once.

The hub deploys the Worker as the script `herder-relay` with a SQLite-backed Durable Object
class and the secret binding `RELAY_SECRET`, enables its `workers.dev` route, and keeps
what it deployed (the account, the URL, the secret and the source's hash) in
`<data_dir>/cloudflare/relay.json`, readable by its user only. It deploys again when its
bundled source changes, so updating herder updates the relay, and when the script is gone
from the account; turning the relay off deletes the script with its Durable Objects.

**Cost.** Every socket is accepted through the WebSocket Hibernation API: between frames a
Durable Object is out of memory and billed nothing, and Cloudflare answers pings without
waking it, so the hosts' idle control sockets cost nothing. On Cloudflare's published
pricing (2026) the Workers Free plan includes SQLite-backed Durable Objects with 100,000
requests a day (each upgrade one; incoming WebSocket messages count 1 in 20) and 13,000
GB-s of duration a day (about 28 hours awake at 128 MB). One person with a phone and a
laptop, a few hours a day, stays well inside it: **free**. Only a household streaming busy
sessions all day would need Workers Paid, $5 a month. Outgoing messages and bandwidth are
not billed.

## Choosing a route

One connection per machine; ranked routes raced with head starts only when a new
connection is needed; never a move mid-use.

- **When**: at connect, at wake (the app back in the foreground), on a drop, and on a
  network change the app reports (a wake). Not while a working connection is in use: no
  probing of better routes to move to (P27.2).
- **How**: the existing race (`supervisor::race`): the machine's addresses in order, each
  started 300 ms after the one before or at once when every earlier one failed, the first
  to finish its hello wins, the losers are dropped before they say one.
- **Last winner first**: the address that answered last starts the race (`dial_order`), so
  a phone that is away keeps going straight to the route that worked; at home the LAN
  address still wins the race it starts in second, 300 ms behind.
- **The user's say**: per machine, `set_addresses` reorders; `set_route` pins one address
  (only it is dialled) or disables one (never dialled, and kept so a daemon's hello does
  not add it back). The apps show which route is in use (`Machine::address`, its
  `RouteKind`) and its round trip (`Machine::quality`).

## Battery

- herder's tunnels (the WireGuard device per imported config or hub enrollment, a relay's
  WebSocket) exist only while a connection or a race uses them. The tunnel's UDP socket is
  opened by the first dial through it and closed 30 s after the last connection on it ends.
  A suspended app (`Client::suspend`) keeps nothing up that it would not already.
- No keepalive of herder's own beyond the connection's pings: WireGuard's
  `PersistentKeepalive` is ignored, the WireGuard handshake rekeys only while there is
  traffic, and the ping is the only steady traffic.
- An idle connection is pinged every 25 s instead of 10 s (P27.2), still finding a dead link
  within P26.12's bounds.

## Security

| Requirement | How |
| --- | --- |
| End-to-end herder TLS on every route | Every carrier carries the same inner TLS 1.3 with the pinned machine certificate and the device certificate; the WireGuard endpoint, the hub relay, the Durable Object and Cloudflare's edge see ciphertext. |
| Relays refuse unauthenticated use | The hub relay forwards only for devices paired with it (P24). The Durable Object Worker forwards only for bearer tokens the hub signed, by kind and path. The hub's WireGuard endpoint answers enrolled keys only. |
| Private keys never leave the device | Device keys, imported WireGuard private keys and hub-enrollment private keys stay in the device's profile (0600). Only public keys travel: `EnrollWireGuard`. |
| Revocation | Revoking a device on the hub drops its WireGuard peer and refuses its relay token renewals; revoking it on a machine refuses it inside every route, as today. |
| No clash with other VPNs | herder's WireGuard is userspace and herder-only: no system profile, routes or DNS. |
| Provider login tokens | Untouched by every route. The Cloudflare API token is a hub secret, used by the hub only. |
| Terminals owner-only | Untouched: roles come from each machine's own auth. |

## Failure modes

| Failure | What the user sees |
| --- | --- |
| The router's WireGuard server is down or its UDP port blocked | The `wg://` route gets no handshake answer and loses its race; other routes answer. Its error says "no WireGuard handshake from <endpoint> in 5 s". |
| At home with the own-VPN route ranked first by the user | The race still starts the LAN address 300 ms later; the tunnel usually fails (hairpin) and the LAN wins. |
| AllowedIPs do not cover a machine | No `wg://` route is added for it. |
| The hub's UDP port is not forwarded | `wg://hub/…` never handshakes; the Hub tab's reach check (P26.3) says so for that route. |
| The device's hub enrollment was revoked | The handshake is ignored; the toggle shows off once the device next reaches the hub and `GetRemoteAccess` says not enrolled. |
| Cloudflare API token wrong or lacking scope | `RemoteAccess::relay`/`tunnel` state `failed` with Cloudflare's message. |
| The Worker is down, or the token expired while away | The `relay+wss://` route fails with the Worker's status (401 names an expired token); it renews on the next connection to the hub. |
| A route drops mid-use | The supervisor's grace and race, as for any drop; one connection, no duplicate. |

## Contract

What every P27 todo builds on. Added in P27.1; optional fields and new commands, so
`PROTOCOL_VERSION` and `REPLICATION_VERSION` stay.

**`herder-protocol::remote`** (commands answered by hubs; a daemon that has none of it
answers `unsupported`):

| Command | Who | Answer |
| --- | --- | --- |
| `GetRemoteAccess` | any client device | `CommandResult::RemoteAccess { remote_access: RemoteAccess }`: `settings` (owners only), `wireguard: Option<WireGuardOffer { endpoint, enrolled }>`, `relay`/`tunnel: Option<RouteStatus { url, state, error }>` |
| `SetRemoteAccess { settings: RemoteAccessSettings }` | owners | the same `RemoteAccess`, as applied |
| `EnrollWireGuard { public_key }` | any client device | `CommandResult::WireGuardEnrolled { enrollment: WireGuardEnrollment { endpoint, hub_public_key, address, hub_address } }` |
| `UnenrollWireGuard` | any client device | `Applied` |
| `GetRelayToken` | any client device | `CommandResult::RelayToken { grant: RelayGrant { url, token, expires_at } }` |

`RemoteAccessSettings { wireguard: Option<WireGuardSettings { port, endpoint }>,
cloudflare: Option<CloudflareSettings { account_id, api_token: SecretRef, relay: bool,
tunnel_hostname: Option<String> }> }`.

**Replication**: `HostHello::cloudflare_relay` (the host keeps a control socket to the hub's
Durable Object relay when granted one), `HubHello::relay_grant: Option<RelayGrant>` and
`HubMessage::RelayGrant { grant }` (sent only to hosts that set it).

**`herder-client-core`** (`CLIENT_API_VERSION` 24):

| Item | P27.1 | Filled in by |
| --- | --- | --- |
| `route_kind(address) -> RouteKind` (`HomeNetwork`, `OwnVpn { tunnel }`, `Direct`, `SystemVpn`, `HubWireGuard`, `Tunnel`, `Hub { hub }`, `CloudflareRelay`) | done | P27.3, P27.5 add their forms to `normalize` |
| `Client::set_route(host_id, address, RouteChoice)` (`Auto`, `Pinned`, `Disabled`), `Machine::pinned_address`, `Machine::disabled_addresses` | done | |
| `Client::import_wireguard(name, config) -> WireGuardTunnel`, `wireguard_tunnels()`, `remove_wireguard(id)`; `WireGuardTunnel { id, name, endpoint, allowed_ips, kind, up }` | parse, validate, store | P27.3: the tunnel, its routes, `up` |
| `Client::set_hub_wireguard(host_id, enabled)` | `Local` error "not available yet" | P27.4 |

## Crates

- **WireGuard: `boringtun` 0.7, `default-features = false`.** Only its `noise::Tunn` (the
  protocol state machine: handshakes, cookies, timers, encapsulate/decapsulate), no device,
  no TUN, no FFI. Cloudflare's, used in WARP; releases in January and May 2026 after a
  quiet 2023-2025; crypto on `ring`, which the workspace already uses. Mullvad's fork
  `gotatun` is more active (0.7 to 0.10 between August and November 2026) but its API churns
  release to release and it pulls `aws-lc-rs` (default), `eyre`, `typed-builder`,
  `windows-sys` and a TUN layer we do not use; `Tunn` is the same protocol either way, so a
  switch later is local to one module. `defguard_boringtun` is a lightly used fork.
- **TCP/IP: `smoltcp` 0.14**, with `medium-ip`, `proto-ipv4`, `proto-ipv6`, `socket-tcp`.
  No kernel interface needed, no allocation per packet, used by onetun and embassy for the
  same job.
- Both live in a new crate, **`herder-wireguard`** (P27.3): a tokio driver that pumps one
  `Tunn` per peer and one `smoltcp::Interface` on a UDP socket, and exposes connected TCP
  streams as `AsyncRead + AsyncWrite` (client: `connect(addr)`; hub: `accept()` for a
  fixed address and port). `herder-client-core` and `herder-daemon` depend on it; nothing
  else does.
- The Durable Object relay is plain JavaScript under `deploy/cloudflare-relay/` (P27.5),
  type-checked with `// @ts-check` and `tsc --noEmit` against `@cloudflare/workers-types`,
  so the hub embeds and uploads the file as it is, with no build step; its tests
  run under `workerd`/miniflare in CI, never against Cloudflare.

## Todos

- **P27.1 · [CONTRACT] Remote access design** (this doc and the contract above).
- **P27.2 · Stay on the route in use; ping less when idle.** Done (#707). Pinned and
  disabled routes came with P27.1 and are tested there.
- **P27.3 · herder's built-in WireGuard client, for any WireGuard server.** Done when a
  client connects to an in-process daemon through a test WireGuard server (in-process,
  `herder-wireguard`) with an imported config, the `wg://` route is added from
  `AllowedIPs`, ranks after the home network and races as designed, the tunnel's socket
  closes when idle, the apps (QR and file) and TUI import a config, and bad-network tests
  cover it.
- **P27.4 · herder's own WireGuard on the hub, one click.** Done when a paired device
  enables it with `set_hub_wireguard` and reaches a machine through `hub://` with the hub
  on `wg://hub/…` in a test; an unenrolled key's handshake gets no answer; revoking a
  device drops its peer; `deploy/hub.yaml` has the UDP port and docs say which port to
  forward.
- **P27.5 · Durable Object relay in the user's Cloudflare account.** Done when herder
  deploys, updates and removes the Worker via the API (tested against a mocked API), a
  local `workerd` relays a session end to end in a test, connections without a valid
  token (missing, expired, wrong kind or host) are refused, tokens renew, and the
  `relay+wss://` route ranks last as designed.
- **P27.6 · Managed Cloudflare Tunnel for the hub.** Done (`herder-daemon::hub::cloudflare`,
  `hub::remote_access`, which keeps the settings for every route): with the settings above:
  done when, with a token and `tunnel_hostname`, herder sets up, repairs and removes the
  tunnel and hostname (mocked API), the hub runs `cloudflared` and publishes the hostname
  as a public address, `RemoteAccess::tunnel` reports its state, and docs cover it.
- **P27.7 · Sign in to a front door with a browser.** Unchanged.
- **P27.8 · Remote access options in the Hub tab.** Unchanged, with `GetRemoteAccess`,
  `set_route`, `route_kind` and the WireGuard calls as its data.

[`RelayGrant`]: ../crates/herder-protocol/src/remote.rs
