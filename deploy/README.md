# deploy

Running the herder vault: the herder daemon in vault mode, which runs no sessions and keeps a
durable copy of every session journal its hosts replicate to it, and the secrets they use. Run
it as a service on any machine with `herder vault install`, or on Kubernetes with the
manifests here:

- `Containerfile`: a minimal image, one static musl `herder` binary on `scratch`.
- `vault.yaml`: a ConfigMap with the vault's `daemon.toml`, a Service, and a one-replica
  StatefulSet whose data dir (TLS identity, paired hosts, `db/vault.db`) lives on a
  10 GiB PersistentVolumeClaim.

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

Once the vault is up, `install` prints a pairing code and the vault's addresses: add it in the
app with Add Machine. It logs to `~/Library/Logs/herder/vault.log` on macOS, and to
`journalctl --user -u herder-vault.service` on Linux.

Run `herder pair --config ~/.local/share/herder-vault/vault.toml` for another pairing code, and
use the same `--config` with every command below that runs `/herder` in the pod.

```sh
herder vault uninstall           # stops and removes the service, keeps its dir
herder vault uninstall --purge   # deletes the dir too: its sessions, secrets and key
```

Reinstalling keeps the dir, so the vault comes back with its devices, sessions and secrets.
`herder update` restarts an installed vault onto the new binary along with the daemon's
service, so the two never run different versions.

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

```sh
kubectl create namespace herder
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
a stolen copy of its device key, sees nothing of the others. It cannot `herder fork`
another host's session either, nor take a session handed off to it, since that reads the
vault, until an owner turns on "Takes handoffs" for it (see [Devices](#devices)).

## Pair a client

`herder pair` without `--host` mints a client code, as on any daemon: the device reads every
host's sessions, read-only. A host that should be able to `herder fork` sessions of other
hosts, or take handoffs, is paired with a client code instead (`/herder pair --user devbox`);
it replicates as well. A host already paired with `--host` needs no new code: turn on "Takes
handoffs" for it instead.

## Devices

`kubectl exec -n herder herder-vault-0 -- /herder pair --list` lists paired devices; the
`ACCESS` column says which are `host` (replicate only) and which `client` (read everything).
`--revoke <device>` unpairs one, and a host revoked this way pairs again with a new code.

In the app, each host on the vault's page has a "Takes handoffs" switch, for the vault's
owners. Turned on, every device the host replicates from may read the vault as a client too,
so the host can fork other hosts' sessions and take handoffs; it stays paired, and its
replication goes on. Turned off, it is host-only again, and its open client connections
close.

Devices paired before host codes existed could both replicate and read. The first time a
vault with host codes starts, every device that has replicated as a host becomes host-only;
the rest stay clients. To let such a host fork other hosts' sessions, turn on "Takes handoffs"
for it.

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
