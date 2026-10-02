# deploy

Kubernetes manifests for running the herder vault: the herder daemon in vault mode, which runs
no sessions and keeps a durable copy of every session journal its hosts replicate to it.

- `Containerfile`: a minimal image, one static musl `herder` binary on `scratch`.
- `vault.yaml`: a ConfigMap with the vault's `daemon.toml`, a Service, and a one-replica
  StatefulSet whose data dir (TLS identity, paired hosts, `db/vault.db`) lives on a
  10 GiB PersistentVolumeClaim.

## Build the image

From the repository root:

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

Mint a one-time code on the vault, named after the host:

```sh
kubectl exec -n herder herder-vault-0 -- /herder pair --user devbox
```

It prints the vault's certificate fingerprint and the code; the addresses it lists are the
pod's, so use the Service's address instead. On the host, add to its `daemon.toml`:

```toml
[vault]
address = "vault.example.com:7447"
fingerprint = "<fingerprint>"
pairing_code = "<code>"
```

then restart the host's daemon (`herder service restart`). The code works once, for ten
minutes; once the host has paired, `pairing_code` is no longer needed. The host replicates
every session's journal from then on, and catches up after either side was offline.

`kubectl exec -n herder herder-vault-0 -- /herder pair --list` lists paired hosts, and
`--revoke <device>` unpairs one.
