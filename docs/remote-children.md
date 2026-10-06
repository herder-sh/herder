# Remote children: spawning a task's child on another machine

Status: accepted by the owner (§7). This document adds no code. Each layer in §6 is its own
todo; the contract changes are their own `[CONTRACT]` todos.

Today `spawn` always starts the child on the primary's host. An agent on `trash-can-01` that
needs the Mac (to build the Apple apps, read the Mac daemon's logs, drive Xcode) can only ask
the user to fork it there. This proposal lets `spawn` name another machine, and the primary
keeps every task tool on that child: `send`, `status`, `wait_for`, `answer`, `escalate`.

## 1. Decisions at a glance

1. **Hosts connect to each other directly, never through the vault.** A vault can be shared
   by several people's hosts; letting it relay commands would let anything that reaches the
   vault drive every host behind it. Only a host's own owner decides which other host may
   start work on it.
2. **A new pairing: peer.** The target's owner runs `herder pair --peer`; the source host
   runs `herder peer add <link>`. One direction per pairing: the Mac pairing `trash-can-01`
   lets `trash-can-01` spawn on the Mac, not the reverse.
3. **A peer can only touch its own children.** A peer connection may create a child of one
   of its host's sessions, then prompt, unarchive, answer for, escalate and follow the
   children it created. Nothing else: no session lists, no other sessions, no forks, no
   uploads, and never a terminal.
4. **The child is the target's session.** It lives in the target's journal and runs on the
   target's clone, worktree and account. The target's own clients see it and can step in like
   any session; its parent is shown as a session on another machine.
5. **The primary's host turns the child's journal into task events.** It follows each remote
   child over the peer connection and writes `child_reported` into the primary's journal,
   and queues the child's requests for the primary, as the local actor does today.
6. **The target caps what peers' children may do.** A remote child runs at most at the
   permission mode of its project's default on the target (`default_permission_mode`), and
   never above the primary's, so the target's owner keeps the last word without new config.

## 2. What the agent sees

`spawn` gains one optional input:

```jsonc
{ "task": "Build the Mac app", "prompt": "…", "machine": "Coingate-CJH9FXV367" }
```

`machine` is a peer's name or host id. Left out, the child runs here as now. The result is
unchanged (`child`, `branch`); the branch is on the target's clone. The other tools take the
child's session id as now: session ids are ULIDs, unique across hosts, so the primary's host
knows which children are remote and where.

Refusals the agent can act on:

- `not_allowed`: no peer by that name, or the peer does not have the primary's project.
- `host_busy` with `retry_after_secs`: the target is unreachable or its admission refuses.
- the existing ones (depth, provider, permission mode).

## 3. Pairing and trust

- `herder pair --peer` on the target mints a peer-only code, as `herder pair --host` mints a
  host-only one for vaults. The paired device gets `DeviceRole::Peer` and a user named after
  the source host (a member), like vault host devices get today.
- `herder peer add <link>` on the source stores the target's name, addresses, fingerprint
  and a peer device key under its data dir, as `[vault]` stores the vault's. `herder peer
  list` and `herder peer remove` look after them; removing on the target is the existing
  device revocation.
- A peer connection says so in its hello, the way a vault replication hello does, and is
  authorised per command against the list in decision 3. Every event it causes is `by` the
  peer user, so the target's journal says which host asked.
- Clients get both sides once the layers below land: owners pair and revoke peers from the
  apps and the TUI, as they link vault hosts today.

## 4. How the child runs

On the source, `spawn` with `machine` first snapshots the primary's worktree as a checkpoint
(`refs/herder/<primary>/spawn-<child>`, with the checkpoint rules on what is left out) and
pushes it to `origin`, waiting for the push: the turn's own checkpoint comes only once the
turn ends. A repository with no `origin`, or a failed push, refuses the spawn as
`not_allowed` with the reason, as a bundle cannot reach another host. It then sends
`create_child` to the target over the peer connection, carrying the checkpoint commit, the primary's session id and host id, its project id, task, provider,
model and permission ceiling, and the hop count agent messages carry.

On the target, `create_child`:

- resolves the project by id on this host (project ids are the same across hosts, e.g.
  `github.com/herder-sh/herder`) and refuses when it is not here;
- picks this host's account for the provider with the most headroom (the lowest of each
  account's highest usage window), so a remote child spreads load across the target;
- fetches the checkpoint the spawn names from `origin` and creates the worktree from it, as
  a fork restores one, so the child starts from the primary's work as it was at the spawn,
  uncommitted changes included; the session is created with
  `parent` naming the remote primary and its host;
- caps its permission mode (decision 6) and records `by` the peer user.

The child's actor routes as now, except that a request for its primary goes onto the peer
connection's queue instead of the local `Tasks` registry. When its turn ends it does not
write `child_reported` anywhere: there is no local parent journal to write to.

On the source, a per-peer client (built on `herder-client-core`, as the vault client is)
subscribes to every live remote child from the source's cursor and:

- writes `child_reported` into the primary's journal when a turn ends, from the child's
  last reply, as `Actor::report` does;
- queues the child's open requests for the primary in `Tasks`, and withdraws them when they
  resolve, so `wait_for` and `status` read one registry for local and remote children;
- sends `send`, `answer` and `escalate` as commands, answered by the target's actor.

If the target is unreachable, the remote child keeps running there; the source reconnects
with backoff and resumes from its cursor, so no report is lost. `status` shows the child as
last known, marked unreachable.

## 5. Contract changes

| Contract | Change |
| --- | --- |
| `herder-protocol` | peer hello; `create_child`, `pair_peer` and `revoke_peer` commands; `parent_host: Option<HostId>` on `session_created`; `host_id: Option<HostId>` on `child_spawned`, so clients open the child on the right machine |
| `herder-store` | a `session_created` with `parent_host` set skips the local-parent check; `sessions.parent_host` column |
| `herder-tasktools` | `spawn` gains `machine` |

## 6. Layers

Each lands on a product that works end to end; the first two change nothing users see.

1. **[CONTRACT] Remote parents in the store and protocol.** `parent_host` on
   `session_created`, the store's relaxed parent check, `host_id` on `child_spawned`.
2. **[CONTRACT] Peer pairing.** `DeviceRole::Peer`, `herder pair --peer`, `herder peer
   add|list|remove`, peer hello and authorisation (everything refused but the hello yet).
3. **[CONTRACT] Spawn on a peer.** `machine` on `spawn`, `create_child`, the target's child
   actor queueing for a remote primary, the source's per-peer client, `child_reported` and
   requests into `Tasks`. Tested with two daemons and the fake adapter.
4. **send, answer, escalate over the peer.** The remaining task tools on remote children.
5. **Clients.** Remote children under their primary in the TUI and apps when the client is
   paired with both machines; a "child on <machine>" card otherwise; peer pairing and
   revocation from the apps.

## 7. Owner decisions

- **Accounts:** the target's account for the provider with the most headroom.
- **Repository state:** the child starts from a checkpoint of the primary's worktree taken at
  the spawn and pushed to `origin`.
- **Pairing:** one `herder pair --peer` per direction; no two-way shortcut.
