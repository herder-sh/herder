# deploy

Running the herder vault: the herder daemon in vault mode, which runs no sessions and keeps a
durable copy of every session journal its hosts replicate to it, and the secrets they use. Run
it as a service on any machine with `herder vault install`, or on Kubernetes with the
manifests here:

- `Containerfile`: a minimal image, one static musl `herder` binary on `scratch`.
- `vault.yaml`: ConfigMaps with the vault's `daemon.toml` and its CLIProxyAPI's config, a
  Service, and a one-replica StatefulSet whose data dir (TLS identity, paired hosts,
  `db/vault.db`) and CLIProxyAPI logins live on a 10 GiB PersistentVolumeClaim. See
  [Provider logins](#provider-logins).

## On a machine

From the app, an owner does it all with **Run a Vault on This Machine** in the machine's Backup
section: its daemon installs the vault as below, links itself to it with Secrets on and Backup
off, and pairs the app with it as an owner, named "‹machine›’s vault". A vault installed by hand
and not linked yet shows **Pair and Link** there instead.

By hand, on a Mac or a Linux box with herder installed, next to its daemon or on its own:

```sh
herder vault install             # listens on port 7448; --port picks another
```

It runs the vault as its own service, a LaunchAgent (`sh.herder.vault`) on macOS and a systemd
user unit (`herder-vault.service`) on Linux, which starts with the machine and restarts when it
stops, apart from the daemon's `herder service`. Everything the vault keeps is in its own dir,
`~/.local/share/herder-vault` (`$XDG_DATA_HOME/herder-vault`):

- `vault.toml`: its config, `mode = "vault"` on every interface at its port; edit it and run
  `herder vault install` again to apply.
- `vault.env`: the secrets key (see [Secrets](#secrets)), generated at the first install and
  readable by you only. Only the vault's service gets it; on macOS it is copied into the
  LaunchAgent's plist, also readable by you only. Back it up with the dir.
- its data: TLS identity, paired devices and hosts, `db/vault.db`.

Once the vault is up, `install` prints a `herder://own` link, as a QR code, and a short code:
scan it with the app and approve with your passkey to become the vault's owner. Your other
devices then sign in with the same passkey, with no code. It logs to
`~/Library/Logs/herder/vault.log` on macOS, and to `journalctl --user -u herder-vault.service`
on Linux.

Run `herder claim --config ~/.local/share/herder-vault/vault.toml` to give the vault a new owner
if you lose every device, and use the same `--config` with every command below that runs
`/herder` in the pod.

```sh
herder vault uninstall           # stops and removes the service, keeps its dir
herder vault uninstall --purge   # deletes the dir too: its sessions, secrets and key
```

Reinstalling keeps the dir, so the vault comes back with its devices, sessions and secrets.
`herder update` restarts an installed vault onto the new binary along with the daemon's
service, so the two never run different versions.

### CLIProxyAPI on a machine

`herder vault install` also runs a [CLIProxyAPI](#provider-logins) next to the vault, holding
the fleet's provider logins, as the Kubernetes pod does. It downloads CLIProxyAPI's pinned
release for the machine from its GitHub Releases, checks it against the SHA-256 herder
pins for it, and runs it as a service of its own: a LaunchAgent (`sh.herder.cliproxyapi`,
logging to `~/Library/Logs/herder/cliproxyapi.log`) on macOS, a systemd user unit
(`herder-cliproxyapi.service`) on Linux. Its files are in the vault's dir:

- `cliproxyapi.env`: the proxy's management secret, generated at the first install and
  readable by you only. Only the vault's service gets it, which manages the proxy with it.
- `cliproxyapi/cli-proxy-api`: the proxy.
- `cliproxyapi/config.yaml`: its config, with the settings of the
  [pod's](#provider-logins) (`fill-first`, session affinity rebinding on quota only,
  `allow-remote: false` with the management secret, never `MANAGEMENT_PASSWORD`). Each
  install writes it again, with a random first client key, and the vault puts its hosts'
  keys back within 30 seconds.
- `cliproxyapi/auth`: the provider logins.

The install sets `cliproxyapi` in `vault.toml`'s `[vault]` table, so the vault mints its hosts'
keys there. Reinstalling keeps the logins and the management secret, and downloads the proxy
again only for a new pinned release. `herder vault uninstall` stops and removes the proxy's
service and binary and keeps its logins; `--purge` deletes them with the rest of the dir.
`herder vault install --no-cliproxyapi` runs none, and removes one an earlier install set up.

**`cliproxyapi/auth` holds every provider login in plain text**, the most sensitive thing
herder has: anyone who can read the vault's dir, or its backups, holds the fleet's logins.
Keep the machine's disk encrypted, and treat backups of the dir as you would the logins.

By default the proxy listens on `127.0.0.1:8317` over plain HTTP, for a host daemon on the same
machine. [Pair that host](#pair-a-host) with the vault, then give it an endpoint account in its
`daemon.toml`, and restart it:

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
this machine here too, such as with `127.0.0.1 cpa.example.com` in `/etc/hosts`, for the vault
manages the proxy at that name and the proxy takes management requests from its own machine only:

```sh
herder vault install --cliproxyapi-tls-cert cpa.crt --cliproxyapi-tls-key cpa.key \
  --cliproxyapi-name cpa.example.com   # listens on 0.0.0.0:8317; --cliproxyapi-listen picks another
```

Hosts then use `endpoint = "https://cpa.example.com:8317"`. The flags are not kept: pass them
again when you reinstall. `herder doctor --config ~/.local/share/herder-vault/vault.toml` says
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

`docker build` works the same way. Set the image in `vault.yaml` to the one you pushed.

### Deploy

`vault.yaml` reads three Secrets, which you create first: the secrets key
([Secrets](#secrets)), and the CLIProxyAPI's management secret and certificate
([Provider logins](#provider-logins)):

```sh
kubectl create namespace herder
kubectl create secret generic -n herder herder-vault-secrets \
  --from-literal=key="$(openssl rand -base64 32)"
kubectl create secret generic -n herder herder-cliproxyapi \
  --from-literal=management-secret="$(openssl rand -hex 32)"
kubectl create secret tls -n herder herder-cliproxyapi-tls --cert=cpa.crt --key=cpa.key
kubectl apply -n herder -f deploy/vault.yaml
kubectl rollout status -n herder statefulset/herder-vault
kubectl get -n herder service herder-vault   # the address hosts connect to
```

## Pair a host

Mint a one-time host code on the vault, named after the host:

```sh
kubectl exec -n herder herder-vault-0 -- /herder pair --host devbox
```

It prints the `[vault]` table for the host; the address in it is the pod's, so use the
Service's address instead. On the host, add to its `daemon.toml`:

```toml
[vault]
address = "vault.example.com:7447"
fingerprint = "<fingerprint>"
pairing_code = "<code>"
```

then restart the host's daemon (`herder service restart`). The code works once, for ten
minutes; once the host has paired, `pairing_code` is no longer needed. The host replicates
every session's journal from then on, and catches up after either side was offline.
`backup = false` in the table stops it replicating, and `secrets = false` stops its sessions
fetching secrets from the vault; both are on by default, and the app switches them on a
running host. A host links to one vault at most.

A host paired with `--host` may only replicate its own sessions. It reads nothing on the
vault: no hosts, sessions, transcripts, PRs, attachments or devices, so a shared machine, or
a stolen copy of its device key, sees nothing of the others. No host ever reads another
host's sessions.

## Sign in

People reach the vault as themselves, with their passkey: its first owner claims it from the
app, and invites the rest. A terminal signs in with `herder login <address> --fingerprint
<hex>`, approved from the app. To fork another host's session onto a host, from the vault
when that host is gone, run `herder fork <session>` on it from a terminal signed in to both
the vault and that host: the terminal reads the session from the vault as you and hands it
over.

## Devices

`kubectl exec -n herder herder-vault-0 -- /herder pair --list` lists paired devices; the
`ACCESS` column says which are `host` (replicate only) and which `client` (read everything).
`--revoke <device>` unpairs one, and a host revoked this way pairs again with a new code.

## Storage

Hosts back up their prompt images only when their `[vault]` table says so:

```toml
[vault]
attachments = true           # off by default
attachments_cap = 1073741824 # bytes the vault keeps of this host's images; 1 GiB by default
```

The vault keeps a host's images within its cap, evicting the oldest first; an image bigger
than the whole cap is not kept. A session recovered from the vault shows "image not backed
up" for each image it does not hold.

Archived sessions of hosts that are online leave the vault 90 days after their latest event.
Set it in the vault's own config:

```toml
mode = "vault"

[vault]
archive_retention_days = 90
```

Nothing else leaves on its own: a host's live sessions stay as long as it has them, and the
sessions of a host that is gone stay until you forget it, dropping its sessions and images
and unpairing it:

```sh
kubectl exec -n herder herder-vault-0 -- /herder vault forget-host devbox
```

A host is forgotten only while it is offline; stop it backing up first. The fleet view shows
each host's sessions and image bytes against its cap, and warns once the vault's disk is
more than 80% full.

## Secrets

The vault keeps secrets that agents use to call services without seeing them: API keys and
tokens, in named sets an owner grants to hosts. Every session on a granted host may run a
command with them through `herder secret run NAME -- command`, which hands the values to the
command in its environment; the agent sees names only.

Values are encrypted at rest under a master key the vault reads from
`HERDER_VAULT_SECRETS_KEY` at start: 32 random bytes, base64. Without it the vault keeps
working, and refuses every secret command. `herder vault install` generates one in
`vault.env`. On Kubernetes, generate one and keep it in a Kubernetes Secret, which `vault.yaml`
reads:

```sh
kubectl create secret generic -n herder herder-vault-secrets \
  --from-literal=key="$(openssl rand -base64 32)"
kubectl rollout restart -n herder statefulset/herder-vault
```

Back the key up with the volume: the values stored under it cannot be read without it, and a
vault started with another key hands none of them out. It is not a `daemon.toml` key, so no
client can read or change it.

## Provider logins

One pool of provider logins for the whole fleet runs next to the vault: a
[CLIProxyAPI](https://github.com/router-for-me/CLIProxyAPI) (MIT), the `cliproxyapi`
container in the vault's pod, `eceasy/cli-proxy-api` pinned to a release. It holds Claude and
Codex logins, and hosts run sessions through it with endpoint accounts, each with its own
client key, which no one types:

1. When a host first backs up to the vault, the vault mints a random key for it, adds it to
   the proxy's `access.api-keys` through the proxy's management API, and keeps it as the
   secret `CLIPROXYAPI_KEY` in a set of its own, `cliproxyapi-<host id>`, granted to that
   host for its own use. Its sessions never get it through `herder secret run`.
2. On the host, an endpoint account names that secret, not the key. With
   `cliproxyapi_endpoint` in the vault's `[vault]` table, the address hosts reach the proxy at,
   the vault tells each host holding a key where the proxy is, and **Add Account** in the apps
   and the TUI offers **Use the vault's logins** for Claude and Codex there, which adds this
   account with nothing typed. By hand, in the host's `daemon.toml`:

   ```toml
   [[accounts]]
   id = "fleet"
   provider = "claude"          # or codex
   endpoint = "https://cpa.example.com:8317"
   key_secret = "CLIPROXYAPI_KEY"
   ```

   The host fetches the key from the vault when a session starts on the account, and keeps
   it in memory for five minutes. With the vault out of reach and no key in memory, the
   session cannot start there: it fails as unreachable, and failover moves it to the host's
   other accounts, such as its own logins or a CLIProxyAPI of its own.
3. Forgetting the host (`herder vault forget-host`) drops its set and takes its key out of
   the proxy at once: it has no access any more.

The vault owns the proxy's client keys: every 30 seconds it puts back the hosts' keys if the
proxy's list differs, so a key added by hand goes, and a restarted proxy gets them back. With
no host, the list holds one random key nobody has, as a proxy with no client key serves
anyone. A vault whose `[vault]` table has no `cliproxyapi` does none of this.

The proxy's settings, in the `herder-cliproxyapi` ConfigMap:

- `routing.strategy: fill-first`: it stays on one login until that hits its limit, and only
  then moves to the next, as herder's own failover is reactive only.
- `routing.session-affinity: true` with `session-affinity-rebind-on: quota`: a session stays
  on its login, and moves on a limit or a refused login only, never on a passing error.
- `management.allow-remote: false`: only the pod itself manages the proxy, with the
  management secret from the `herder-cliproxyapi` Secret. The proxy hashes it on start. Never
  give the proxy `MANAGEMENT_PASSWORD` instead: the proxy takes that one from any address,
  whatever `allow-remote` says.
- `oauth.auth-dir` is on the vault's volume, so logins survive restarts.

An init container copies the ConfigMap's config onto the volume at each start, with the
management secret and a random first client key filled in, as the proxy writes its config
back. Settings changed through the proxy's management API last until the next restart.

### TLS

Hosts reach the proxy at the Service's port 8317, and the proxy serves TLS itself, with a
certificate from the `herder-cliproxyapi-tls` Secret. That is the simplest correct option:
the Claude and Codex CLIs check the server against the CAs they trust and cannot pin a
certificate, so the vault's own self-signed certificate, which hosts pin, cannot serve them,
and a TLS proxy in front would be one more piece to run. Get the certificate from a CA the
hosts trust, for the name hosts use, such as with cert-manager into that Secret, and put
that name in place of `cpa.example.com` in `vault.yaml`: in `daemon.toml`, where the vault
reaches the proxy's management API, and in `hostAliases`, which points the name at the pod
itself, so the certificate checks out there too and the proxy sees a request from localhost.

### The most sensitive thing herder has

The proxy keeps every provider login as an OAuth file in plain text in
`/data/cliproxyapi/auth` on the vault's volume. Anyone who can read that volume, or exec into
the pod, holds the fleet's logins: restrict who can `exec` into, attach to, or snapshot the
vault's pod and PersistentVolumeClaim, encrypt the volume at rest, and treat its backups as
you would the logins themselves.

### Log the proxy in

Until the proxy holds a login, sessions on it fail. An owner adds logins from the Mac or
iPhone app: on the vault's page, under **Logins**, choose **Add login**, pick Claude or Codex,
and sign in in the browser. The provider then sends the browser to a `localhost` address that
does not load, as the proxy is not on your machine; copy that address from the address bar
and paste it into the app, which hands it to the proxy. The proxy trades the code in it for
the login and keeps the tokens; herder never sees them. The same page lists the logins with
whether each is in use, cooling down, or needs signing in again, and removes them.

Or log it in from the pod, forwarding the provider's OAuth callback port to your machine,
here Claude's (54545):

```sh
kubectl port-forward -n herder herder-vault-0 54545:54545 &
kubectl exec -it -n herder herder-vault-0 -c cliproxyapi -- \
  /CLIProxyAPI/CLIProxyAPI -config /data/cliproxyapi/config.yaml -claude-login -no-browser
```

Open the URL it prints. `-codex-device-login` logs in to Codex with a device code and needs no
port. Add as many logins as you have; the proxy fills one, then the next.

`herder doctor` in the vault's container says whether the proxy answers and how many logins
it holds:

```sh
kubectl exec -n herder herder-vault-0 -c vault -- /herder doctor
```
