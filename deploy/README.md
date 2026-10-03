# deploy

Kubernetes manifests for running the herder vault: the herder daemon in vault mode, which runs
no sessions and keeps a durable copy of every session journal its hosts replicate to it.

- `Containerfile`: a minimal image, one static musl `herder` binary on `scratch`.
- `vault.yaml`: a ConfigMap with the vault's `daemon.toml`, a Service, and a one-replica
  StatefulSet whose data dir (TLS identity, paired hosts, `db/vault.db`) lives on a
  10 GiB PersistentVolumeClaim.

## Build the image

CI publishes a multi-arch (amd64, arm64) image to `ghcr.io/herder-sh/herder`: `:latest`
and `:main` track `main`, `:sha-<short>` pins a commit, and a `v*` tag adds `:<version>`.

To build your own, from the repository root:

```sh
podman build -f deploy/Containerfile -t registry.example.com/herder:dev .
podman push registry.example.com/herder:dev
```

`docker build` works the same way. Set the image in `vault.yaml` to the one you pushed.

## Deploy

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

A host paired with `--host` may only replicate its own sessions. It reads nothing on the
vault: no hosts, sessions, transcripts, PRs, attachments or devices, so a shared machine, or
a stolen copy of its device key, sees nothing of the others. It cannot `herder recover`
either, since that reads the vault.

## Pair a client

`herder pair` without `--host` mints a client code, as on any daemon: the device reads every
host's sessions, read-only. A host that should be able to `herder recover` sessions of dead
hosts is paired with a client code instead (`/herder pair --user devbox`); it replicates as
well.

## Devices

`kubectl exec -n herder herder-vault-0 -- /herder pair --list` lists paired devices; the
`ACCESS` column says which are `host` (replicate only) and which `client` (read everything).
`--revoke <device>` unpairs one, and a host revoked this way pairs again with a new code.

Devices paired before host codes existed could both replicate and read. The first time a
vault with host codes starts, every device that has replicated as a host becomes host-only;
the rest stay clients. To let such a host keep recovering, revoke it and pair it again
with a client code.
