# deploy

Running the herder hub: the herder daemon in hub mode, which runs no sessions and keeps a
durable copy of every session journal its hosts replicate to it, and the secrets they use. Run
it as a service on any machine with `herder hub install`, or on Kubernetes with the
manifests here:

- `Containerfile`: a minimal image, one static musl `herder` binary on `scratch`.
- `hub.yaml`: ConfigMaps with the hub's `daemon.toml` and its CLIProxyAPI's config, a
  Service, and a one-replica StatefulSet whose data dir (TLS identity, paired hosts,
  `db/hub.db`) and CLIProxyAPI logins live on a 10 GiB PersistentVolumeClaim. See
  [Provider logins](#provider-logins).

## On a machine

From the app, an owner does it all with **Run a Hub on This Machine** in the machine's Backup
section: its daemon installs the hub as below, links itself to it with Secrets on and Backup
off, and pairs the app with it as an owner, named "‹machine›’s hub". A hub installed by hand
and not linked yet shows **Pair and Link** there instead.

By hand, on a Mac or a Linux box with herder installed, next to its daemon or on its own:

```sh
herder hub install             # listens on port 7448; --port picks another
```

It runs the hub as its own service, a LaunchAgent (`sh.herder.hub`) on macOS and a systemd
user unit (`herder-hub.service`) on Linux, which starts with the machine and restarts when it
stops, apart from the daemon's `herder service`. Everything the hub keeps is in its own dir,
`~/.local/share/herder-hub` (`$XDG_DATA_HOME/herder-hub`):

- `hub.toml`: its config, `mode = "hub"` on every interface at its port; edit it and run
  `herder hub install` again to apply.
- `hub.env`: the secrets key (see [Secrets](#secrets)), generated at the first install and
  readable by you only. Only the hub's service gets it; on macOS it is copied into the
  LaunchAgent's plist, also readable by you only. Back it up with the dir.
- its data: TLS identity, paired devices and hosts, `db/hub.db`.

Once the hub is up, `install` prints a `herder://own` link, as a QR code, and a short code:
scan it with the app and approve with your passkey to become the hub's owner. Your other
devices then sign in with the same passkey, with no code. It logs to
`~/Library/Logs/herder/hub.log` on macOS, and to `journalctl --user -u herder-hub.service`
on Linux.

Run `herder claim --config ~/.local/share/herder-hub/hub.toml` to give the hub a new owner
if you lose every device, and use the same `--config` with every command below that runs
`/herder` in the pod.

```sh
herder hub uninstall           # stops and removes the service, keeps its dir
herder hub uninstall --purge   # deletes the dir too: its sessions, secrets and key
```

Reinstalling keeps the dir, so the hub comes back with its devices, sessions and secrets.
`herder update` restarts an installed hub onto the new binary along with the daemon's
service, so the two never run different versions.

### CLIProxyAPI on a machine

`herder hub install` also runs a [CLIProxyAPI](#provider-logins) next to the hub, holding
the fleet's provider logins, as the Kubernetes pod does. It downloads CLIProxyAPI's pinned
release for the machine from its GitHub Releases, checks it against the SHA-256 herder
pins for it, and runs it as a service of its own: a LaunchAgent (`sh.herder.cliproxyapi`,
logging to `~/Library/Logs/herder/cliproxyapi.log`) on macOS, a systemd user unit
(`herder-cliproxyapi.service`) on Linux. Its files are in the hub's dir:

- `cliproxyapi.env`: the proxy's management secret, generated at the first install and
  readable by you only. Only the hub's service gets it, which manages the proxy with it.
- `cliproxyapi/cli-proxy-api`: the proxy.
- `cliproxyapi/config.yaml`: its config, with the settings of the
  [pod's](#provider-logins) (`fill-first`, session affinity rebinding on quota only,
  `allow-remote: false` with the management secret, never `MANAGEMENT_PASSWORD`). Each
  install writes it again, with a random first client key, and the hub puts its hosts'
  keys back within 30 seconds.
- `cliproxyapi/auth`: the provider logins.

The install sets `cliproxyapi` in `hub.toml`'s `[hub]` table, so the hub mints its hosts'
keys there. Reinstalling keeps the logins and the management secret, and downloads the proxy
again only for a new pinned release. `herder hub uninstall` stops and removes the proxy's
service and binary and keeps its logins; `--purge` deletes them with the rest of the dir.
`herder hub install --no-cliproxyapi` runs none, and removes one an earlier install set up.

**`cliproxyapi/auth` holds every provider login in plain text**, the most sensitive thing
herder has: anyone who can read the hub's dir, or its backups, holds the fleet's logins.
Keep the machine's disk encrypted, and treat backups of the dir as you would the logins.

By default the proxy listens on `127.0.0.1:8317` over plain HTTP, for a host daemon on the same
machine. The install sets both `cliproxyapi` and `cliproxyapi_endpoint` in the hub's
`[hub]` table to the proxy's address, so the apps offer **Use the hub's logins** in Add
Account. [Pair that host](#pair-a-host) with the hub, then add the account from the app, or
give it an endpoint account in its `daemon.toml` and restart it:

```toml
[[accounts]]
id = "fleet"
provider = "claude"              # or codex
endpoint = "http://127.0.0.1:8317"
key_secret = "CLIPROXYAPI_KEY"
```

For hosts on other machines, the proxy serves TLS, with a certificate from a CA the hosts trust,
for the name they reach it at. The Claude and Codex CLIs check the server against the CAs they
trust and cannot pin a certificate, so a self-signed one does not work. The name must resolve to
this machine here too, such as with `127.0.0.1 cpa.example.com` in `/etc/hosts`, for the hub
manages the proxy at that name and the proxy takes management requests from its own machine only:

```sh
herder hub install --cliproxyapi-tls-cert cpa.crt --cliproxyapi-tls-key cpa.key \
  --cliproxyapi-name cpa.example.com   # listens on 0.0.0.0:8317; --cliproxyapi-listen picks another
```

Hosts then use `endpoint = "https://cpa.example.com:8317"`. The flags are not kept: pass them
again when you reinstall. `herder doctor --config ~/.local/share/herder-hub/hub.toml` says
whether the proxy answers and how many logins it holds; the [Logins](#log-the-proxy-in) tab in
the apps adds them.

## On Kubernetes

### Build the image

CI publishes a multi-arch (amd64, arm64) image to `ghcr.io/herder-sh/herder`: `:latest`
and `:main` track `main`, `:sha-<short>` pins a commit, and a `v*` tag adds `:<version>`.

To build your own, from the repository root:

```sh
podman build -f deploy/Containerfile -t registry.example.com/herder:dev .
podman push registry.example.com/herder:dev
```

`docker build` works the same way. Set the image in `hub.yaml` to the one you pushed.

### Deploy

`hub.yaml` reads three Secrets, which you create first: the secrets key
([Secrets](#secrets)), and the CLIProxyAPI's management secret and certificate
([Provider logins](#provider-logins)):

```sh
kubectl create namespace herder
kubectl create secret generic -n herder herder-hub-secrets \
  --from-literal=key="$(openssl rand -base64 32)"
kubectl create secret generic -n herder herder-cliproxyapi \
  --from-literal=management-secret="$(openssl rand -hex 32)"
kubectl create secret tls -n herder herder-cliproxyapi-tls --cert=cpa.crt --key=cpa.key
kubectl apply -n herder -f deploy/hub.yaml
kubectl rollout status -n herder statefulset/herder-hub
kubectl get -n herder service herder-hub   # the address hosts connect to
```

The manifest's NetworkPolicy lets the hub's pod reach only the cluster's DNS and the
internet, never the LAN, the nodes or another pod, and lets in only its own ports. It needs
a network plugin that enforces NetworkPolicy: k3s's does, unless started with
`--disable-network-policy`.

### A public hostname

The hub can be the one thing you expose: apps and hosts reach it at a public hostname
through a Cloudflare Tunnel, and reach every host through it (`docs/hub-relay.md`).

#### The managed tunnel

The hub can make the tunnel and the hostname in your Cloudflare account itself, and run
`cloudflared` for them in its own container, so there is nothing else to deploy
(`docs/remote-access.md`). You need a zone on Cloudflare (`example.com`), the account's id
(on its overview page in the dashboard), and an API token: **My Profile → API Tokens →
Create Token → Create Custom Token**, with

- Account · Cloudflare Tunnel · Edit
- Zone · Zone · Read, and Zone · DNS · Edit, for the zone

The hub keeps the token as one of its secrets, so it needs its secrets key
(`HERDER_HUB_SECRETS_KEY`, see [Secrets](#secrets)). Then, in the pod:

```sh
kubectl exec -it -n herder herder-hub-0 -c hub -- /herder hub tunnel set-up \
  --account-id <account id> --hostname hub.example.com
```

It asks for the token (or reads `CLOUDFLARE_API_TOKEN`) and puts it as the hub secret
`cloudflare/CLOUDFLARE_API_TOKEN`; an owner can do the same from the apps, putting the
token as a secret and choosing it in the hub's remote access settings
(`set_remote_access`). For the zone the hostname is in, the hub creates a tunnel
`herder-hub.example.com`, routes the hostname to its tunnel listener, points the hostname at
the tunnel with a proxied CNAME, downloads the `cloudflared` release herder pins (checked
against its SHA-256) onto its volume, and runs it with the tunnel's token, giving it nothing
else of its environment. While it runs, the hub adds `wss://hub.example.com` to the
addresses it names to devices, which pick it up when they next connect: nothing to add under
**Public addresses**. The tunnel's token is kept in `/data/herder/cloudflare/tunnel.json`,
readable by the hub only; neither token ever goes to a client.

Each time the hub starts, it runs `cloudflared` at once and checks the tunnel with
Cloudflare, making again whatever was deleted: the tunnel, its route, the DNS record. It
tries again every five minutes while that fails, and says why in the hub's remote access
status and in `herder hub tunnel status`. Run `set-up` again with another `--hostname` to
move it (Enter keeps the token), and `remove` to stop `cloudflared` and delete the tunnel and
the DNS record:

```sh
kubectl exec -n herder herder-hub-0 -c hub -- /herder hub tunnel status
kubectl exec -n herder herder-hub-0 -c hub -- /herder hub tunnel remove
```

`set-up` refuses a hostname that already has a record of another kind; a CNAME to another
tunnel, such as a hand-made one for the same hostname, it points at its own, so you can move
from a hand-run `cloudflared` and then delete that one.

Cloudflare Access in front of the hostname is yours to add, and never needed for security:
every connection is authenticated by the hub inside the tunnel. Make a self-hosted Access
application for the hostname with a Service Auth policy and a service token, then enter its
two headers once on each device under **Proxy headers…** of the hub's `wss://` address
(`docs/public-access.md`). A device behind Access signs in to the hub at another address
first.

The hub needs `tunnel` set (the manifest's `0.0.0.0:7448`; `cloudflared` in the pod reaches
it on `127.0.0.1`), and egress to `api.cloudflare.com`, `github.com` (once, for the
download) and Cloudflare's edge on port 7844. On a machine, set `tunnel = "127.0.0.1:7449"`
in `hub.toml`, run `herder hub install` again, and use `herder hub tunnel --config
~/.local/share/herder-hub/hub.toml set-up …`.

#### Your own cloudflared

The manifest also serves the tunnel listener as the Service `herder-hub-tunnel`, and lets
only the `cloudflared` namespace connect to it. Add the hostname to your `cloudflared`'s
ingress, ahead of its catch-all rule:

```yaml
ingress:
  - hostname: hub.example.com
    service: http://herder-hub-tunnel.herder.svc.cluster.local:7448
```

and route its DNS to the tunnel (`cloudflared tunnel route dns <tunnel> hub.example.com`,
or a public hostname in the Zero Trust dashboard). `http://` is right: every connection
runs its own TLS inside, pinned to the hub's certificate and carrying the device's, so
Cloudflare relays ciphertext. Leave the hostname outside Cloudflare Access.

On a machine, set `tunnel = "127.0.0.1:7448"` in the hub's `daemon.toml` and point the
machine's own `cloudflared` at it, as for a daemon (`docs/public-access.md`).

With your own `cloudflared`, add `https://hub.example.com` once under **Public addresses**
on the hub's page in the app (or `public_addresses = ["wss://hub.example.com"]` in the hub's config): every device
picks it up when it next connects. A port forward or a TCP proxy works as well as a tunnel
(`docs/public-access.md`). A host that is not
always on the hub's network, a laptop for one, sets `address = "wss://hub.example.com"`
in its `[hub]` table and reaches the hub there from anywhere. Clients paired with the
hub and with a linked host then reach that host through the hub whenever they cannot
dial it; `relay = false` in the host's `[hub]` table turns that off.

### herder's WireGuard

For a hub at home with no VPN of your own and no tunnel: the hub runs a userspace WireGuard
endpoint on one UDP port (no privileges, no TUN device), and each device turns it on with
one switch (`docs/remote-access.md`). Inside it a device reaches the hub's port 7447 and,
through the hub, every machine; nothing else on your network.

1. **Forward one UDP port** on your router to the hub: 51900 to the load balancer's
   address (the manifest's `herder-hub` Service carries it as `wireguard`), or to the
   machine running `herder hub install`. The endpoint answers no packet from a key it
   has not enrolled, so a scan sees a closed port.
2. **Turn it on** as an owner: in the app, Hub tab › Remote access › herder's WireGuard,
   with the port (51900) and where devices reach it from outside, your router's public
   name or IP and that port, such as `home.example.net:51900`. That is
   `set_remote_access`'s `wireguard`; the hub keeps it in `remote-access.json` and its
   WireGuard key in `wireguard.key`, both in its data dir.
3. **Use it** on each device: the switch on the same card. The device makes its own key,
   gives the hub only the public half, and from then on reaches the hub through
   WireGuard whenever its other routes do not answer. Revoking the device drops its key.

#### Keep the port clear of the nodes' own

The port must be one nothing on any node uses. On k3s's built-in ServiceLB (klipper), a
LoadBalancer Service's every port becomes a host port on **every node**: each node
redirects all incoming traffic on that port into the Service's `svclb` pod, whatever on
the node was using it. Other load balancers that bind host ports do the same.

So never give the hub UDP 51820 or 51821 on a cluster. They are the ports of flannel's
`wireguard-native` backend (k3s's `--flannel-backend=wireguard-native`, IPv4 and IPv6),
of Calico's WireGuard encryption and of most WireGuard setups; Cilium's is 51871. Taken
by the hub's Service, every node hands the cluster's own node-to-node tunnel packets to
the hub's pod, and pod traffic between nodes stops cluster-wide. Removing the port does
not undo it at once: the redirect's conntrack entries outlive it while the tunnel's
keepalives refresh them, so flush them (`conntrack -D -p udp --orig-port-dst <port>` on
each node) or reboot the nodes.

The manifest uses 51900. Before you publish it, or any other port, check each node:

```sh
sudo ss -uanp 'sport = :51900'   # nothing listed: free
sudo wg show all listen-port     # the WireGuard interfaces' ports, flannel-wg's among them
```

The hub refuses a port something already holds where it runs, naming it (a WireGuard
interface, say), but in a pod it sees only the pod's own network: the nodes' ports are
out of an unprivileged pod's sight, so this check is yours.

#### Apply to a hub already running

On the live hub, whose Kubernetes objects keep the `herder-vault` names (namespace
`herder`), on k3s with ServiceLB and flannel's `wireguard-native` backend (which holds
UDP 51820 on every node), first check the nodes as above. Then add the port to the
StatefulSet's `hub` container, the Service and the NetworkPolicy, and forward UDP 51900
on the router to the Service's address:

```sh
kubectl -n herder patch statefulset herder-vault --type json -p '[
  {"op": "add", "path": "/spec/template/spec/containers/0/ports/-",
   "value": {"name": "wireguard", "containerPort": 51900, "protocol": "UDP"}}]'
kubectl -n herder patch service herder-vault --type json -p '[
  {"op": "add", "path": "/spec/ports/-",
   "value": {"name": "wireguard", "port": 51900, "targetPort": "wireguard", "protocol": "UDP"}}]'
kubectl -n herder get networkpolicy        # if the hub has one, add the port to its ingress:
kubectl -n herder patch networkpolicy herder-vault --type json -p '[
  {"op": "add", "path": "/spec/ingress/0/ports/-", "value": {"port": 51900, "protocol": "UDP"}}]'
kubectl -n herder get service herder-vault  # the address to forward UDP 51900 to
```

If WireGuard was turned on at 51820 before, change its port to 51900 in the Hub tab
(Change…) and the router's forward with it.

Check first that container 0 is `hub` (`kubectl -n herder get sts herder-vault -o
jsonpath='{.spec.template.spec.containers[*].name}'`) and use its index if not. The
StatefulSet patch restarts the pod, so apply it when no session needs the hub; then an
owner turns WireGuard on in the app as in step 2. A LoadBalancer Service with both TCP and
UDP ports needs Kubernetes 1.26 or later (k3s's ServiceLB handles it).

## Pair a host

Mint a one-time host code on the hub, named after the host:

```sh
kubectl exec -n herder herder-hub-0 -- /herder pair --host devbox
```

It prints the `[hub]` table for the host; the address in it is the pod's, so use the
Service's address instead. On the host, add to its `daemon.toml`:

```toml
[hub]
address = "hub.example.com:7447"
fingerprint = "<fingerprint>"
pairing_code = "<code>"
```

then restart the host's daemon (`herder service restart`). The code works once, for ten
minutes; once the host has paired, `pairing_code` is no longer needed. The host replicates
every session's journal from then on, and catches up after either side was offline.
`backup = false` in the table stops it replicating, and `secrets = false` stops its sessions
fetching secrets from the hub; both are on by default, and the app switches them on a
running host. A host links to one hub at most.

A host paired with `--host` may only replicate its own sessions. It reads nothing on the
hub: no hosts, sessions, transcripts, PRs, attachments or devices, so a shared machine, or
a stolen copy of its device key, sees nothing of the others. No host ever reads another
host's sessions.

## Sign in

People reach the hub as themselves, with their passkey: its first owner claims it from the
app, and invites the rest. A terminal signs in with `herder login <address> --fingerprint
<hex>`, approved from the app. To fork another host's session onto a host, from the hub
when that host is gone, run `herder fork <session>` on it from a terminal signed in to both
the hub and that host: the terminal reads the session from the hub as you and hands it
over.

## Devices

`kubectl exec -n herder herder-hub-0 -- /herder pair --list` lists paired devices; the
`ACCESS` column says which are `host` (replicate only) and which `client` (read everything).
`--revoke <device>` unpairs one, and a host revoked this way pairs again with a new code.

## Storage

Hosts back up their prompt images only when their `[hub]` table says so:

```toml
[hub]
attachments = true           # off by default
attachments_cap = 1073741824 # bytes the hub keeps of this host's images; 1 GiB by default
```

The hub keeps a host's images within its cap, evicting the oldest first; an image bigger
than the whole cap is not kept. A session recovered from the hub shows "image not backed
up" for each image it does not hold.

Archived sessions of hosts that are online leave the hub 90 days after their latest event.
Set it in the hub's own config:

```toml
mode = "hub"

[hub]
archive_retention_days = 90
```

Nothing else leaves on its own: a host's live sessions stay as long as it has them, and the
sessions of a host that is gone stay until you forget it, dropping its sessions and images
and unpairing it:

```sh
kubectl exec -n herder herder-hub-0 -- /herder hub forget-host devbox
```

A host is forgotten only while it is offline; stop it backing up first. The fleet view shows
each host's sessions and image bytes against its cap, and warns once the hub's disk is
more than 80% full.

## Secrets

The hub keeps secrets that agents use to call services without seeing them: API keys and
tokens, in named sets an owner grants to hosts. Every session on a granted host may run a
command with them through `herder secret run NAME -- command`, which hands the values to the
command in its environment; the agent sees names only.

Values are encrypted at rest under a master key the hub reads from
`HERDER_HUB_SECRETS_KEY` at start: 32 random bytes, base64. Without it the hub keeps
working, and refuses every secret command. `herder hub install` generates one in
`hub.env`. On Kubernetes, generate one and keep it in a Kubernetes Secret, which `hub.yaml`
reads:

```sh
kubectl create secret generic -n herder herder-hub-secrets \
  --from-literal=key="$(openssl rand -base64 32)"
kubectl rollout restart -n herder statefulset/herder-hub
```

Back the key up with the volume: the values stored under it cannot be read without it, and a
hub started with another key hands none of them out. It is not a `daemon.toml` key, so no
client can read or change it.

## Provider logins

One pool of provider logins for the whole fleet runs next to the hub: a
[CLIProxyAPI](https://github.com/router-for-me/CLIProxyAPI) (MIT), the `cliproxyapi`
container in the hub's pod, `eceasy/cli-proxy-api` pinned to a release. It holds Claude and
Codex logins, and hosts run sessions through it with endpoint accounts, each with its own
client key, which no one types:

1. When a host first backs up to the hub, the hub mints a random key for it, adds it to
   the proxy's `access.api-keys` through the proxy's management API, and keeps it as the
   secret `CLIPROXYAPI_KEY` in a set of its own, `cliproxyapi-<host id>`, granted to that
   host for its own use. Its sessions never get it through `herder secret run`.
2. On the host, an endpoint account names that secret, not the key. With
   `cliproxyapi_endpoint` in the hub's `[hub]` table, the address hosts reach the proxy at,
   the hub tells each host holding a key where the proxy is, and **Add Account** in the apps
   and the TUI offers **Use the hub's logins** for Claude and Codex there, which adds this
   account with nothing typed. By hand, in the host's `daemon.toml`:

   ```toml
   [[accounts]]
   id = "fleet"
   provider = "claude"          # or codex
   endpoint = "https://cpa.example.com:8317"
   key_secret = "CLIPROXYAPI_KEY"
   ```

   The host fetches the key from the hub when a session starts on the account, and keeps
   it in memory for five minutes. With the hub out of reach and no key in memory, the
   session cannot start there: it fails as unreachable, and failover moves it to the host's
   other accounts, such as its own logins or a CLIProxyAPI of its own.
3. Forgetting the host (`herder hub forget-host`) drops its set and takes its key out of
   the proxy at once: it has no access any more.

The hub owns the proxy's client keys: every 30 seconds it puts back the hosts' keys if the
proxy's list differs, so a key added by hand goes, and a restarted proxy gets them back. With
no host, the list holds one random key nobody has, as a proxy with no client key serves
anyone. A hub whose `[hub]` table has no `cliproxyapi` does none of this.

The proxy's settings, in the `herder-cliproxyapi` ConfigMap:

- `routing.strategy: fill-first`: it stays on one login until that hits its limit, and only
  then moves to the next, as herder's own failover is reactive only.
- `routing.session-affinity: true` with `session-affinity-rebind-on: quota`: a session stays
  on its login, and moves on a limit or a refused login only, never on a passing error.
- `management.allow-remote: false`: only the pod itself manages the proxy, with the
  management secret from the `herder-cliproxyapi` Secret. The proxy hashes it on start. Never
  give the proxy `MANAGEMENT_PASSWORD` instead: the proxy takes that one from any address,
  whatever `allow-remote` says.
- `oauth.auth-dir` is on the hub's volume, so logins survive restarts.

An init container copies the ConfigMap's config onto the volume at each start, with the
management secret and a random first client key filled in, as the proxy writes its config
back. Settings changed through the proxy's management API last until the next restart.

### Through the hub

Take `cliproxyapi_endpoint` out of the hub's `[hub]` table and the hub forwards each
host to the proxy itself, inside the host's own connection to the hub. A host then runs
the hub's logins as accounts wherever it reaches the hub, a laptop away from home
included, and nothing but the hub needs an address: port 8317 can come off the Service.
Hosts must run herder 0.25 or later first; an older one gets no logins from a hub that
names no endpoint. The rest of this section is for a hub that keeps the endpoint.

### TLS

Hosts reach the proxy at the Service's port 8317, and the proxy serves TLS itself, with a
certificate from the `herder-cliproxyapi-tls` Secret. That is the simplest correct option:
the Claude and Codex CLIs check the server against the CAs they trust and cannot pin a
certificate, so the hub's own self-signed certificate, which hosts pin, cannot serve them,
and a TLS proxy in front would be one more piece to run. Get the certificate from a CA the
hosts trust, for the name hosts use, such as with cert-manager into that Secret, and put
that name in place of `cpa.example.com` in `hub.yaml`: in `daemon.toml`, where the hub
reaches the proxy's management API, and in `hostAliases`, which points the name at the pod
itself, so the certificate checks out there too and the proxy sees a request from localhost.

### The most sensitive thing herder has

The proxy keeps every provider login as an OAuth file in plain text in
`/data/cliproxyapi/auth` on the hub's volume. Anyone who can read that volume, or exec into
the pod, holds the fleet's logins: restrict who can `exec` into, attach to, or snapshot the
hub's pod and PersistentVolumeClaim, encrypt the volume at rest, and treat its backups as
you would the logins themselves.

### Log the proxy in

Until the proxy holds a login, sessions on it fail. An owner adds logins from the Mac or
iPhone app: on the hub's page, under **Logins**, choose **Add login**, pick Claude or Codex,
and sign in in the browser. The provider then sends the browser to a `localhost` address that
does not load, as the proxy is not on your machine; copy that address from the address bar
and paste it into the app, which hands it to the proxy. The proxy trades the code in it for
the login and keeps the tokens; herder never sees them. The same page lists the logins with
whether each is in use, cooling down, or needs signing in again, and removes them.

Or log it in from the pod, forwarding the provider's OAuth callback port to your machine,
here Claude's (54545):

```sh
kubectl port-forward -n herder herder-hub-0 54545:54545 &
kubectl exec -it -n herder herder-hub-0 -c cliproxyapi -- \
  /CLIProxyAPI/CLIProxyAPI -config /data/cliproxyapi/config.yaml -claude-login -no-browser
```

Open the URL it prints. `-codex-device-login` logs in to Codex with a device code and needs no
port. Add as many logins as you have; the proxy fills one, then the next.

`herder doctor` in the hub's container says whether the proxy answers and how many logins
it holds:

```sh
kubectl exec -n herder herder-hub-0 -c hub -- /herder doctor
```
