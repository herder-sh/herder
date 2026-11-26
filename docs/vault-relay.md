# The vault relays to every machine

Goal: expose one thing, the vault, at a public hostname, and reach every machine through it
from any network: no VPN, no Tailscale, no open port and no `cloudflared` on the machines,
laptops behind NAT included. Same security as a direct connection, and no worse on a bad
network.

Basecamp: P24 · The vault relays to every machine. Builds on
[public-access.md](public-access.md) (P23).

## What exists

- A client reaches a daemon over TLS 1.3 with the daemon's pinned certificate and its own
  device certificate, then a WebSocket and a hello. Through a Cloudflare Tunnel the same
  connection runs inside the binary frames of an outer WebSocket (P23), so Cloudflare
  relays ciphertext.
- Every host that is linked to a vault keeps one outbound replication connection to it
  (`herder-protocol::replication`). The vault never sends a host a command.
- The vault's CLIProxyAPI serves hosts' endpoint accounts at an address hosts must be able
  to dial (`cliproxyapi_endpoint`).

## Design

```
app ──TLS(vault cert, device cert) + WS──▶ vault ◀──TLS(vault cert, host's device cert) + WS── host
 │       relay hello: connect <host>         │           relay hello: accept <stream>           │
 └────────── TLS 1.3 (pinned host cert + device cert) + WebSocket + hello, end to end ──────────┘
```

1. **The app asks the vault for a stream.** It opens an ordinary connection to the vault
   (direct, or through the vault's tunnel URL), authenticated as every vault connection is,
   and says `RelayHello::Connect { host_id }` instead of a client hello.
2. **The vault asks the host to dial in.** It sends `VaultMessage::RelayOpen { stream_id,
   device_id }` on the host's replication connection. The host opens a second, outbound
   connection to the vault with its own device key and says `RelayHello::Accept
   { stream_id }`.
3. **The vault splices the two.** It answers both `RelayAnswer::Ready`; from then on both
   connections carry binary frames only, and the vault copies each frame's bytes to the
   other side.
4. **The app and the host run a direct connection inside.** The app starts TLS pinning the
   host's certificate and presenting its device certificate, then the WebSocket and hello.
   The host hands the stream to the same `serve` path a TCP socket or a tunnel's frames get.

The wire types are in `herder-protocol::relay`; schemas `relay_hello.json` and
`relay_answer.json`.

### The vault only forwards

[remote-children.md](remote-children.md) decision 1 holds: a vault shared by several
people's hosts must not be able to drive them. Here it cannot:

- The inner TLS is between the app and the host. The vault holds neither's key, so it reads
  and alters nothing; a vault that tried to answer for the host fails the pin.
- The host authenticates the device by the certificate presented inside the stream and
  authorizes each command by that device's role, exactly as on a direct connection. Roles,
  passkeys and owner-only terminals are unchanged. `device_id` in `RelayOpen` is the vault's
  name for the device, used by the host for caps and logs only.
- `VaultMessage::RelayOpen` asks the host for one thing: an outbound connection. It carries
  no command and no session data.

What the vault decides is whether to forward at all:

| Check | Refusal |
| --- | --- |
| The device is paired with the vault as a client (not a host device) | `forbidden` |
| The host replicates to this vault and its hello set `relay` | `unknown_host` |
| The host is connected, and opens its end within 10 s | `host_offline` |
| Streams open: at most 32 per device, 64 per host | `busy` |

A host that is not connected gets 3 s to appear before `host_offline`: after a vault
restart the app and the host both reconnect, and the app must not lose that race.

A host opts out with `[vault] relay = false`. The default is on: the route only widens who
can *knock* (devices the vault knows, instead of devices on the LAN), never who gets in.

### Why a connection per stream, not a multiplexed replication connection

The replication connection carries the request to open a stream, never its bytes.

- **Flow control is TCP's.** Each stream is its own connection on both legs, so a slow
  phone slows only its own replay. Multiplexing would need stream ids and credit windows
  written and tested by us, and the vault and host loops would have to stop handling one
  message at a time, which today is what makes a batch durable before its ack.
- **Nothing shares a queue.** A 16 MB attachment in replication cannot delay a keystroke
  in a relayed terminal, and one stream's lost packet stalls only that stream.
- **Failures stay small.** A replication reconnect leaves open streams alone. A stream
  that breaks takes nothing else with it.
- **The vault's work is a copy loop.** One frame read, one frame written, back-pressure
  from the socket. No buffering beyond the frame in hand.

The cost is one more connection setup when a stream opens (TCP, TLS, upgrade, hello from
host to vault). It is paid once per app connection, and the host is usually a millisecond
from the vault.

### Layers and cost

Through the public hostname a message is wrapped, outermost first: Cloudflare's TLS, the
tunnel's WebSocket, the vault's TLS and WebSocket, the host's TLS and WebSocket. That is
about 100 bytes and two extra AES-GCM passes per message; no extra round trip once the
stream is open. Opening costs about three more round trips than a direct connection. P24.3
measures the forwarding overhead in a test.

### How an app addresses a machine through the vault

A machine's address list (`Machine::addresses`) gains one form: `vault://<vault host id>`,
"through that vault". Client-core dials it by opening a connection to the vault machine of
that id in its own profile (racing the vault's addresses as usual, its tunnel URL included)
and asking for a stream to the machine.

- **Automatic.** When a vault lists a host with `FleetHost::relay` and the client has that
  host as a machine, client-core appends the vault's route to the machine's addresses if it
  is missing. Nothing to set up in the apps.
- **Last by default.** The route ranks after tunnel URLs: LAN, other, Tailscale, tunnel,
  vault. The user can reorder it like any address.
- **Fastest working route.** The existing race applies: direct addresses get their head
  start, the vault route answers when they do not. While connected through a later address
  the supervisor already probes earlier ones and moves back; that covers returning home.
- **Reconnects.** A relayed connection is a connection: the supervisor's grace, backoff and
  resume are unchanged, so a host or vault reconnect shows what a dropped direct connection
  shows and no more. P24.7 puts the relayed route through the bad-network harness.

`Machine::address` names the route in use, so the apps and the TUI can say "through
<vault>".

A device must already be paired with the host. Signing in to a new machine through the
vault needs links that name the vault route, which waits on P23.6.

### How a host reaches the vault from anywhere

The vault takes tunneled connections like a daemon does (`tunnel` in its config), so its
public hostname serves apps and hosts alike. A host's `[vault] address` may be a tunnel URL:

```toml
[vault]
address = "wss://vault.example.com"
```

A host that never leaves the vault's network keeps `host:port`. One that moves uses the
URL everywhere: replication is light, and at home apps still reach it directly.

In a cluster `cloudflared` is another pod, not loopback, so a vault's `tunnel` listener may
bind a non-loopback address (a host's still may not). What that gives up is the trust in `CF-Connecting-IP` behind the
per-client cap on unauthenticated connections; the total cap and the inner TLS do not
depend on it. `deploy/vault.yaml` restricts the port to the `cloudflared` namespace with a
NetworkPolicy, and the vault logs a warning for a non-loopback `tunnel`.

### The vault's CLIProxyAPI from anywhere

Only the vault is exposed, so the proxy is reached through it too:

- A vault with `cliproxyapi` and no `cliproxyapi_endpoint` sets
  `VaultHello::cliproxyapi_relay`.
- The host already runs its vault logins' CLIs through a proxy of its own on loopback,
  which pins each request to its login. For each request that proxy now opens a relay
  connection to the vault with `RelayHello::Proxy` and speaks HTTP on it; the vault serves
  it, forwarding the request to its proxy (speaking the proxy's TLS itself when it has one)
  and streaming the answer back.
- The vault forwards everything but the proxy's management API, which stays reachable
  from the vault only.
- The proxy still checks the host's client key. The vault process sees that traffic in the
  clear, as the proxy beside it does; Cloudflare sees ciphertext. Nothing reads or stores a
  provider login.

A vault that sets `cliproxyapi_endpoint` keeps telling hosts that address, as today, and
forwards none. A host older than this ignores `cliproxyapi_relay`, so take the endpoint out
of a vault's config only once its hosts are updated. The alternative, a second public hostname for the proxy, would put it on the internet behind a
bearer key and show every prompt to Cloudflare.

Only a host device whose host holds a client key may open proxy streams, at most 64 at once.

## Is it safe on the public internet?

| Requirement | Status |
| --- | --- |
| No plaintext at the vault or Cloudflare | Inner TLS 1.3 between app and host. They see sizes and timing. |
| The vault cannot drive a host | It forwards bytes it cannot read; `RelayOpen` only asks for a connection. |
| Only known devices reach a host's handshake | The vault forwards for its paired client devices only, to hosts linked to it that opted in. |
| Host auth unchanged | Same `serve` path: device certificate, hello within 10 s, no command before the server hello. |
| Unauthenticated connections bounded | The vault's tunnel gate (P23.5) for strangers; per-device and per-host stream caps; the host counts relayed connections not yet signed in per vault device, as its tunnel gate does per client address. |
| Revocation | Revoking a device on the host works as before. Revoking it on the vault closes the route. |
| Provider login tokens | Untouched. |
| Terminals owner-only | Untouched: roles come from the host's own auth. |

## Contracts

| Change | Where |
| --- | --- |
| `herder-protocol::relay`: `RelayHello`, `RelayAnswer`, `RelayError`, `RELAY_VERSION` | this todo |
| `HostHello::relay`, `VaultMessage::RelayOpen`, `VaultHello::cliproxyapi_relay` | this todo; optional, so `REPLICATION_VERSION` stays 1 |
| `FleetHost::relay` | this todo; optional, so `PROTOCOL_VERSION` stays 5 |
| `vault://` addresses in client-core | No signature change: `set_addresses` and the saved profile take more strings, as with tunnel URLs. |
| `tunnel` on a vault, `[vault] relay`, `[vault] address` as a URL | Config and daemon internals. |

Old and new builds mix: an old host sets no `relay`, so the vault sends it nothing new and
lists it without a route; an old vault skips the field and never asks.

## Todos

- **P24.1 · [CONTRACT] Vault relay design and protocol** (this doc and the types).
- **P24.2 · The vault forwards relay streams.** The relay hellos on the vault, its checks
  and caps, the splice, `FleetHost::relay`, and the vault's tunnel listener with the
  `deploy/vault.yaml` port and NetworkPolicy. Done when a paired client device and a host
  that accepted exchange bytes both ways through the vault unchanged, each refusal in the
  table above is returned for its case, closing one end closes the other, and a vault
  serves a client through its tunnel listener, all tested.
- **P24.3 · Hosts take relayed connections.** `[vault] relay`, answering `RelayOpen`,
  serving the stream as a direct connection, and `[vault] address` as a tunnel URL. Done
  when a client that can reach only the vault streams a fake-adapter session
  from a daemon through it, an unpaired device and a wrong pinned fingerprint are refused
  through it as directly, a host with `relay = false` is not listed as relayed, and a test
  reports the round-trip overhead of the relayed route over the direct one.
- **P24.4 · Client-core reaches machines through their vault.** `vault://` addresses, added
  automatically and ranked last, dialled through the vault. Done when a client whose
  machine's direct address is dead connects through the vault, moves back to the direct
  address once it answers, and the route appears in the machine's addresses only after the
  vault lists the host as relayed, all tested.
- **P24.5 · Apps and TUI name the vault route.** Done when the machine page and the address
  list show "through <vault name>" for a `vault://` address instead of the raw string, in
  the TUI and both Apple apps, with tests.
- **P24.6 · Hosts reach the vault's CLIProxyAPI through the vault.** Done when a host whose
  vault sets `cliproxyapi_relay` completes an HTTP request to a fake proxy through its
  loopback listener and the vault, a device that is not such a host is refused, and a vault
  with `cliproxyapi_endpoint` behaves as before, all tested.
- **P24.7 · Bad-network test for the relayed route.** Done when the bad-network harness
  runs its faults on the app-to-vault and vault-to-host legs of a relayed connection, a host
  or vault reconnect loses and duplicates nothing and shows no more than a direct drop
  does, and the test gates every PR.
