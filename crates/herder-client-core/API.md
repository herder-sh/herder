# herder-client-core public API

`CLIENT_API_VERSION = 10`

This is the reviewed reference for the API the TUI, the `herder` CLI and the native apps
(SwiftUI, GTK4, Compose) build on. The rustdoc of each item is the detailed contract; this file
says what exists, why, and how it maps to foreign languages.

## Stability

- The surface is every public item reachable from the crate root. `public-api.txt` lists it
  (signatures, fields, variants, derives; no docs or bodies), and the `public_api` test fails
  when the code and the list differ.
- A change that can break a client (anything removed, renamed, or changed in its fields,
  variants, derives or signature) bumps `CLIENT_API_VERSION` by one. Additions keep it. The
  test refuses to rewrite the list for a breaking change unless the version went up.
- To change the API on purpose: change the code, update this file, bump the version if it
  breaks, then run
  `UPDATE_PUBLIC_API=1 cargo test -p herder-client-core --test public_api` and commit
  `public-api.txt`. Such a change is a `[CONTRACT]` todo of its own (AGENTS.md).
- The API passes `herder-protocol` types through (`HostId`, `SessionHead`, `Event`,
  `CommandBody`, ...). Those are versioned by `PROTOCOL_VERSION` and change only in a
  protocol `[CONTRACT]` todo; this list names them but does not track their fields. Protocol
  version 4 (P0.10) changed them without changing this API: `Account.failover` is gone,
  `CreateSession` takes a `provider` and an optional `permission_mode`, and there are new
  commands and results for images, folders, projects and unarchiving. P0.12 added the
  `RemoveProject` command, a compatible addition that keeps both versions. P0.14 added
  `Project.icon` and the `GetProjectIcon` command, compatible too. P0.13 added, also
  compatibly, the `ForkSession` command with its `SessionForked` result, and a
  vault's `VaultStatus` message (`VaultStatus`, `HostReplication`). P0.20 added, compatibly,
  `SessionHead.queue` (`QueuedPrompt`, `PromptId`) and the `RemoveQueued`, `MoveQueued` and
  `SendQueuedNow` commands. The `SetResourceLimits` command, with `MAX_TURNS_LIMIT`, was
  added compatibly too. Handing a session off between machines added, compatibly, the
  `UploadHistory` command (`HistoryPart`) and `ForkSession`'s `relay` (`Relay`), and so were the `GetSettings`, `SetSettings` and
  `RestartDaemon` commands with their `Settings` result (`DaemonSettings`). Uploading a
  project's icon added, compatibly, the `SetProjectIcon` command and `Project.icon_uploaded`. The `MergeQueued`
  command was added compatibly too. P11.5 added, compatibly, the skill library: the
  `SetSkillsRepo`, `PutSkill` (`SkillFile`), `DeleteSkill`, `ImportSkill`, `PullSkills` and
  `SetSkillEnabled` commands, and the `SkillsStatus` (`LibrarySkill`, `ProviderReload`,
  `SkillReload`) and `SessionSkills` (`SessionSkill`, `SkillSource`) messages. P11.1 added,
  compatibly, `TurnCompleted.usage` (`TurnUsage`) and the `GetUsageSummary` command with its
  `UsageSummary` result (`UsagePeriod`, `UsageTotal`). `Project.icon_background` and the `icon_background` of
  `SetProjectSettings` were added compatibly too.

## Shape, and how it maps to UniFFI

The API is designed so that the UniFFI layer (P6.2, for Swift and Kotlin) wraps it without
adapting anything:

| Rust                                                          | UniFFI                          |
| ------------------------------------------------------------- | ------------------------------- |
| `Client`, `SessionSubscription`, `TerminalStream`, `Changes`  | objects (`Arc`, `Send + Sync`)  |
| `Machine`, `ConnectionQuality`, `SessionUpdate`, `NewAccount`, `PairingUri`, `PairingLink`, `SharedLink`, `SkippedMachine` | records |
| `ConnectionState`, `TerminalEvent`, `PairResult`              | enums with named fields         |
| `Error`                                                       | error enum with named fields    |
| `async fn` methods                                            | async methods on a tokio runtime (`async_runtime = "tokio"`) |
| protocol newtype ids (`HostId`, `SessionId`, ...)             | custom types over `String`      |
| other protocol types                                          | remote records and enums        |

Rules the surface keeps, and `public_api` checks the object rules:

- Every argument and return value is owned: `String`, `Vec`, `Option`, `HashMap`, primitive
  integers, records, enums. No references beyond `&self`, no generics, no lifetimes, no
  `PathBuf` (`Client::open` takes the config dir as a `String`).
- Objects and every future their methods return are `Send`; objects are `Send + Sync`.
- Streams are pull-based objects with an `async fn next(&self)` that returns `None` (or
  `false`) when it ends. They need no foreign callback interface; Swift wraps one in an
  `AsyncSequence`, Kotlin in a `Flow`. Dropping (in Swift/Kotlin: releasing) the object
  unsubscribes or detaches.
- `Client::open` must be called within a tokio runtime; the FFI layer owns that runtime.
- `PairingUri` and `PairingLink` parse with `FromStr` and format with `Display`. UniFFI
  cannot export trait impls on records, so the FFI layer exports them as plain functions.

The `auth` module is Rust-only plumbing (TLS device keys and certificate pinning) for whatever
else connects to a daemon as a device: the vault's replicator in `herder-daemon`, and tests.
It is part of the frozen list but not of the foreign-language surface.

## Coverage by area

Everything a client does is one of: read the machines list, stream a session, send a command,
stream a terminal. Commands are `herder_protocol::CommandBody` values sent with
`Client::send`, so a new command is a protocol change, not a client-core change. Queries
(`GetAttachment`, `ListDirectory`) are commands too: their answer is the `CommandResult`, and
the daemon does not remember it, so a resend after a reconnect asks again.

| Area      | Read                                                                 | Act                                                                                       |
| --------- | -------------------------------------------------------------------- | ----------------------------------------------------------------------------------------- |
| Machines  | `Client::machines`, `Client::changes`, `Machine::connection`, `quality`, `role` | `Client::pair`, `share`, `rename`, `set_addresses`, `reconnect`, `forget`, `suspend`, `wake`, `synced`; `PairingUri`, `PairingLink` |
| Sessions  | `Machine::sessions`, `Client::subscribe_session` → `SessionUpdate`; a `UserMessage`'s `attachments`; `SessionHead::queue` | `send`: `CreateSession` (by account, by provider, or the project's default), `ArchiveSession`, `UnarchiveSession`, `SendPrompt` (with `images`), `GetAttachment` → `CommandResult::Attachment`, `Interrupt`, `RemoveQueued`, `MoveQueued`, `SendQueuedNow`, `MergeQueued` (see Prompt queue), `SetModel`, `SetPermissionMode`, `ComposeDown` |
| Projects  | `Machine::projects`; a `Project`'s `icon`, the hash of its icon, to cache it by, `icon_uploaded`, whether an owner uploaded it, and `icon_background`, the colour behind it | anyone: `send`: `GetProjectIcon` → `CommandResult::ProjectIcon` (`not_found` when it has none; fetch again when `icon` changes). Owners: `send`: `ListDirectory` → `CommandResult::Directory`, `AddProject` → `CommandResult::ProjectAdded`, `SetProjectSettings`, `RemoveProject` (refused with `conflict` while it has live sessions; deletes nothing on disk), `SetProjectIcon` (an `Image` of one of `PROJECT_ICON_MEDIA_TYPES`, at most `MAX_PROJECT_ICON_BYTES`, else `bad_request`; `None` goes back to the icon found in the clone) |
| Approvals | `ApprovalRequested` / `QuestionAsked` / `…Escalated` / `…Resolved` / `QuestionAnswered` events; `SessionHead::children_need_you` | `send`: `AnswerApproval`, `AnswerQuestion`                                               |
| Terminals | `Machine::terminals`; `TerminalStream::next` → `TerminalEvent`       | `Client::open_terminal`, `attach_terminal`; `TerminalStream::input`, `resize`; drop = detach |
| PRs       | `PrLinked` / `PrUpdated` / `PrUnlinked` events                       | `send`: `LinkPr`, `UnlinkPr`                                                              |
| Accounts  | `Machine::accounts`, `failover` (the pin; every account takes part in rotation); `AccountSwitched` / `ProviderSwitched` events | `Client::add_account` with `NewAccount` (a login terminal), `log_in_account` (a login terminal for an account whose login expired); `send`: `SwitchAccount`, `SwitchProvider` |
| Skills    | `Machine::skills` (the library as that daemon has it: `repo`, `head`, `last_pull`, `pull_error`, each skill's `name`, `description`, whether `enabled` there and the `providers` it reaches, and per provider when a running session sees a change: `live`, `next_turn` or `next_session`), `Machine::session_skills` (per live session, the library and project skills its agent may use, each with its `source`) | owners: `send`: `SetSkillsRepo`, `PutSkill`, `DeleteSkill`, `ImportSkill`, `PullSkills`, `SetSkillEnabled` (per machine); members are refused with `forbidden`. A write commits and pushes through the one daemon it is sent to. The client keeps every machine where the user is owner on one library: an accepted `SetSkillsRepo` is sent on to the others, and to any that connects later naming another repository or none (as one paired since); a repository set from another device on a connected machine becomes the library; after an accepted write every other machine gets `PullSkills`, at once or once it reconnects. Each machine's `skills` (`head`, `last_pull`, `pull_error`) shows where it stands |
| Fleet     | `Machine::hosts` (a vault), `Machine::vault` (what it holds of each host, live), `SessionHead::host_id`, `Machine::projects`, `resources`, `session_usage` | read-only: a vault rejects commands with `read_only`. To fork any session (its host up or gone) onto a host, call `Client::fork_session` with the machine that lists it and the destination (owners of the destination only) → `CommandResult::SessionForked`; the new session joins the destination's list, the original is left as it is. It forks a session of the destination from its own journal, else relays the history from the session's machine while that is connected, else lets the destination read its vault |

## Reference

### Constants

- `CLIENT_API_VERSION: u32` — the version of this API.

### `Client` (object)

The client: paired machines and one connection supervisor per machine. Cheap to clone;
everything stops once the last clone is dropped.

| Method | Does |
| ------ | ---- |
| `open(config_dir: String, client: String) -> Result<Client, Error>` | Opens the profile in `config_dir`, starts from its offline cache, and starts connecting to every saved machine. `client` names the client in daemon logs. |
| `machines() -> Vec<Machine>` | Every paired machine, in pairing order. |
| `changes() -> Changes` | Notifications that `machines()` changed. |
| `async pair(link: String) -> Result<Vec<PairResult>, Error>` | Pairs with every machine a `herder://pair` link names, at once, and saves each that paired; one `PairResult` per machine, in the link's order. Fails only with `InvalidLink`. |
| `async share() -> Result<SharedLink, Error>` | Asks every connected machine for a one-time code (`pair_device`) and builds one link that pairs another device with all of them, as this device's user with its role on each. A machine not connected, or that refuses or does not answer within 10 s, is skipped. Fails with `Pairing` when no machine gave a code. |
| `rename(host_id: HostId, name: String) -> Result<(), Error>` | Shows a machine as `name` on this device. |
| `set_addresses(host_id: HostId, addresses: Vec<String>) -> Result<(), Error>` | Connects to a machine at `addresses`, in this order of preference, from now on; each a host or IP with an optional port (7447 by default). Fails with `Error::Local` on an invalid address or an empty list. |
| `async reconnect(host_id: HostId) -> Result<String, Error>` | Drops a machine's connection, if up, and connects again at once, racing its addresses in order: the address the new connection uses, or `Unreachable` when none answered. |
| `forget(host_id: HostId) -> Result<(), Error>` | Unpairs a machine on this device. |
| `async synced(host_id: HostId) -> Result<(), Error>` | Waits until a machine is connected and has sent everything owed for what was sent before. |
| `suspend()` | The app went to the background: saves the offline cache (blocking) and stops retrying lost connections. |
| `wake()` | The app is in the foreground: reconnects every disconnected machine now and probes every connected one, replacing a dead connection. |
| `subscribe_session(host_id: HostId, session_id: SessionId) -> Result<SessionSubscription, Error>` | Streams a session, cached state first, across reconnects. |
| `async send(host_id: HostId, command: CommandBody) -> Result<CommandResult, Error>` | Sends a command and waits for the answer; resent with the same id after a reconnect. |
| `async open_terminal(host_id: HostId, session_id: SessionId, cols: u16, rows: u16) -> Result<TerminalStream, Error>` | Opens a shell in a session's worktree; owners only. |
| `async add_account(host_id: HostId, account: NewAccount, cols: u16, rows: u16) -> Result<TerminalStream, Error>` | Runs a provider login in a login terminal; owners only. |
| `async log_in_account(host_id: HostId, account_id: AccountId, cols: u16, rows: u16) -> Result<TerminalStream, Error>` | Runs the provider login of an existing account again, in its own config dir, in a login terminal; owners only. `bad_request` for an unknown account. |
| `async attach_terminal(host_id: HostId, terminal_id: TerminalId) -> Result<TerminalStream, Error>` | Attaches to an open terminal; owners only, one stream per terminal per client. |

### Streams (objects)

- `Changes::next() -> bool` (async) — `true` once the machines changed, coalescing; `false`
  once the client stops.
- `SessionSubscription::next() -> Option<SessionUpdate>` (async) — the next update; `None`
  once the client or the machine stops. Dropping it unsubscribes.
- `TerminalStream` — `next() -> Option<TerminalEvent>` (async), `terminal_id() -> TerminalId`,
  `input(data: Vec<u8>)`, `resize(cols: u16, rows: u16)`. Dropping it detaches; the shell keeps
  running.

### Records

- `Machine` — `host_id`, `name`, `addresses` (in order of preference), `address:
  Option<String>` (the one the current connection uses; `None` while not connected),
  `fingerprint`, `connection`, `quality`, `role`,
  `sessions`, `hosts`, `projects`, `accounts`, `failover`, `terminals`, `resources`,
  `session_usage`, `vault` (`Option<VaultStatus>`: a vault's totals and per-host replication;
  `None` for a daemon and while not connected), `skills` (`Option<SkillsStatus>`: the skill
  library as the daemon has it; `None` until it sends it and while not connected),
  `session_skills` (`HashMap<SessionId, Vec<SessionSkill>>`: the skills each live session's
  agent may use; empty while not connected).
- `ConnectionQuality` — `connected_since: Option<Timestamp>` (when the current connection
  was established; `None` while not connected), `reconnects: u32` (connections established
  after the first, since the client opened), `last_rtt_ms`, `average_rtt_ms`, `min_rtt_ms`,
  `max_rtt_ms: Option<u32>` (round trips of the last 20 pongs on the current connection, in
  milliseconds; `None` until one came back), `missed_pongs: u32` (pings whose pong did not come
  back before the next ping was due, late or lost, since the client opened).
- `SessionUpdate` — `events: Vec<Event>` (new durable events, in seq order) and
  `streaming: Vec<Item>` (every item streaming now; replaces the previous list).
- `NewAccount` — `account_id`, `provider`, `label: Option<String>`, `config_dir: Option<String>`.
- `PairingUri` — one machine of a link: `hosts: Vec<String>`, `fingerprint: String`,
  `code: String`; `FromStr` (fails with `Error::InvalidLink`, also for a link of several
  machines) and `Display` (`herder://pair?…`).
- `PairingLink` — `machines: Vec<PairingUri>`, never empty; `FromStr` (fails with
  `Error::InvalidLink`) and `Display`. See [Pairing links](#pairing-links).
- `SharedLink` — what `share()` made: `link: PairingLink`, `shared: Vec<HostId>` (the
  machines it pairs with, in its order), `skipped: Vec<SkippedMachine>`, `expires_at:
  Timestamp` (when its first code stops working).
- `SkippedMachine` — `host_id`, `error: String` (not connected, refused, or no answer).

### Enums

- `ConnectionState` — `Connecting`, `Connected`, `Disconnected { error: String }`.
- `TerminalEvent` — `Output { data: Vec<u8> }`, `Reattached`, `Closed { exit_code: Option<i32> }`.
- `PairResult` — `Paired { machine: Machine }`, `Failed { addresses: Vec<String>, error:
  String }` (nothing was saved for that machine).
- `Error` — `InvalidLink { message }`, `Pairing { message }`, `UnknownMachine { host_id }`,
  `Rejected { info: ErrorInfo }` (the daemon refused; `info.code` says why, e.g. `forbidden`,
  `read_only`), `Unreachable { message }` (no address answered a `reconnect`), `Local {
  message }`, `Closed`.

### `auth` (Rust-only)

- `DeviceKey` — `generate()`, `from_pem(&str)`, `to_pem() -> &str`, `fingerprint() -> String`.
- `client_config(daemon_fingerprint: &str, device: &DeviceKey) -> anyhow::Result<rustls::ClientConfig>`.

## App lifecycle

An app calls `suspend()` when it goes to the background and `wake()` when it returns (the TUI
calls `wake()` on its reconnect key).

- Suspended, the client keeps connections that are up for as long as the OS lets the process
  run, but does not retry one it loses.
- `wake()` reconnects disconnected machines at once. A connected one is checked: if it was
  silent for longer than 45 s, as after a long suspension in which the OS may have killed the
  socket without either end noticing, it is replaced right away; otherwise it is pinged and
  replaced if the pong does not come back within 5 s. A healthy connection is kept.
- Subscriptions resume from the last seq held, so a suspension of any length shows no gap.

The offline cache lives in `<config_dir>/cache/`, one private file per machine: the role, the
session, host, project and account lists, and every event of the 20 listed sessions with the
newest events. `open` starts from it, so `machines()` and a subscription's first update show
the last known state at once, offline too. Live data always wins: the cache is read only when
a machine's supervisor starts, the daemon's lists replace the cached ones and its events extend
the cached ones by seq. It is saved every 30 s while something changed, when a connection
ends, and on `suspend()`; `forget` deletes it.

## Addresses

A machine's addresses are in order of preference. Connecting races them: each starts 300 ms
after the one before, or as soon as every attempt so far failed, and the first to finish its
hello wins. So the first address wins whenever it answers, and a dead one delays the next by
300 ms, not by the 10 s connect timeout.

`pair` completes each address of the link (the default port, brackets around IPv6) and saves
them direct routes first: private network addresses (home, or a VPN into it such as UniFi
Teleport), then other addresses, then Tailscale ones (`100.64.0.0/10`, `fd7a:115c:a1e0::/48`,
`*.ts.net`). After that `set_addresses` sets the order, and nothing re-sorts it.

A connection that is up moves to an address that comes before its own once one answers: on
`wake()`, which an app also calls when the network changes, and on `set_addresses`. One that
uses an address `set_addresses` dropped is replaced at once.

`reconnect` replaces the connection now, whatever answers first: an app connects through a
chosen address by putting it first with `set_addresses` and then calling `reconnect`. A
dead first address falls back to the next after its 300 ms head start, so the answer says
which address the connection ended up on.

## Connection quality

A connection is pinged as soon as it is up and every 15 s after. Each ping carries a fresh
payload and only its own pong answers it, so `Machine::quality` holds true round trips: the
latest, and the mean, shortest and longest of the last 20, all cleared when the connection
ends. A ping still unanswered when the next is due counts in `missed_pongs`; the probe a
`wake()` sends counts its round trip too. `connected_since` and `reconnects` say how stable
the connection is. Each pong updates the machine, so `Changes` fires about every 15 s per
connected machine.

## Pairing links

A `herder://pair` link names one or more machines. Each is a group of query parameters: one
or more `host` (`host:port`, tried in order), `fp` (SHA-256 of the daemon's certificate,
lowercase hex) and `code` (its one-time pairing code). A group is complete once it has all
three, and the next parameter after a complete group starts the next machine. `herder pair`
prints a link of one group, which parses unchanged:

```
herder://pair?host=192.168.1.5%3A7447&fp=<hex>&code=ABCDE-FGHJK
herder://pair?host=192.168.1.5%3A7447&fp=<hex>&code=ABCDE-FGHJK&host=10.0.0.9%3A7447&host=%5Bfd00%3A%3A9%5D%3A7447&fp=<hex>&code=MNPQR-STVWX
```

`Client::share` builds the second kind: each daemon answers `pair_device` with a code that
pairs a new device as the sharer's own user with the sharer's role (so never more than the
sharer may do; members still get no terminals) and the addresses that daemon advertises,
not the ones the sharer reached it on. The new device gets its own key on every machine,
revocable on its own; the sharer's keys never leave it. The share is one-time: machines
paired later are not passed on.

## Changes in version 10

| Before | Now | Why |
| ------ | --- | --- |
| — | `Machine::skills: Option<SkillsStatus>`, `Machine::session_skills: HashMap<SessionId, Vec<SessionSkill>>` | Apps show the skill library, its sync state on each machine, and the skills each session has. New fields break code that builds a `Machine`. |

## Changes in version 9

| Before | Now | Why |
| ------ | --- | --- |
| — | `Client::reconnect` | Users switch a machine to a chosen address now, and see whether it answered. |
| — | `Error::Unreachable { message }` | `reconnect` says why no address answered. A new variant breaks an exhaustive `match`. |

## Changes in version 8

| Before | Now | Why |
| ------ | --- | --- |
| — | `Client::fork_session` | Hand a session off to another machine without a vault: the client relays its history from the machine it runs on. |
| — | `Client::set_addresses` | Users add addresses (a Tailscale name, a port-forward) and choose which route comes first. |
| — | `Machine::address: Option<String>` | Apps show which route the connection uses. A new field breaks code that builds a `Machine`. |

## Changes in version 7

| Before | Now | Why |
| ------ | --- | --- |
| `pair(link) -> Result<Machine, Error>` | `pair(link) -> Result<Vec<PairResult>, Error>` | A link may name several machines; each pairs or fails on its own. |
| — | `share() -> Result<SharedLink, Error>`, `PairingLink`, `SharedLink`, `SkippedMachine`, `PairResult` | Pair another device with every machine this one has, from one QR code. |

## Changes in version 6

| Before | Now | Why |
| ------ | --- | --- |
| — | `Account::config_dir: Option<String>` | Owners see and change where an account's configuration lives. A new field breaks code that builds an `Account`. |

## Changes in version 4

| Before | Now | Why |
| ------ | --- | --- |
| — | `Machine::vault: Option<VaultStatus>` | Apps show what a vault holds and how far behind each host's replication is. A new field breaks code that builds a `Machine`. |

## Changes in version 3

| Before | Now | Why |
| ------ | --- | --- |
| — | `Machine::quality: ConnectionQuality` | Apps show each machine's latency and stability, to diagnose a slow or flaky connection. |
| a connection is first pinged 15 s after it is up | at once, with a payload its pong is matched by | The round trip is known as soon as the connection is. |

## Changes in version 2

| Before | Now | Why |
| ------ | --- | --- |
| — | `Client::suspend()` | Apps tell the client they went to the background: it saves the offline cache and stops retrying. |
| `wake()` reconnects disconnected machines | it also probes connected ones and replaces a dead connection | A socket the OS killed during a suspension looks connected until the daemon is asked. |
| a new `Client` starts empty | it starts from the offline cache | Apps open instantly, offline too. |

## Changes in version 1

Version 1 is the first frozen API. Compared with the code before it:

| Before | Now | Why |
| ------ | --- | --- |
| `&HostId`, `&SessionId`, `&TerminalId` arguments | owned `HostId`, `SessionId`, `TerminalId` | UniFFI passes records and custom types by value. |
| `Client::open(config_dir: PathBuf, ..)` | `config_dir: String` | UniFFI has no path type. |
| `Changes::next() -> Option<()>` | `-> bool` | `Option<()>` has no foreign equivalent. |
| `Error::InvalidLink(String)`, `Pairing(String)`, `Local(String)`, `UnknownMachine(HostId)`, `Rejected(ErrorInfo)` | named fields: `{ message }`, `{ host_id }`, `{ info }` | Foreign enums get named, not positional, fields. |
| `TerminalEvent::Output(Vec<u8>)` | `Output { data }` | Same. |
| `auth::PairingUri`, `FromStr::Err = anyhow::Error` | `PairingUri` at the crate root, `Err = Error` (`InvalidLink`) | Apps parse links to confirm them before pairing, as the TUI does; `auth` is Rust-only. |
| — | `CLIENT_API_VERSION`, `public-api.txt`, `#![warn(missing_docs)]` | The freeze itself. |

Provider-native sub-agent transcript items carry `Item::parent_call_id`, the `ItemId` of
 their spawning tool call in the same turn. `None` identifies the main conversation.
Clients must group these items beneath their call instead of mixing their text into the
main conversation. This reference is preserved in durable events, offline caches and
streaming snapshots. Claude emits completed nested messages live; nested token deltas
are not currently exposed. These are parts of the parent session, not separate sessions.

### Agent message attribution (client API 5)

`Item.agent_message` is optional authenticated same-host sender metadata: sender session,
caller-chosen delivery ID, trusted relay hop count, and a permission ceiling retained while
queued. The daemon alone sets it on user-message items, with no human `Event.by`. Provider
items cannot set it. Clients label these prompts as sent by another agent and must not match
them against pending human outbox entries. Older human prompts omit this field.

The MCP `send_session` tool accepts a destination session, text and stable `message_id`.
It does not create a child or change task ancestry. Its acceptance means persisted normal
queue delivery; it promises neither immediate execution nor an automatic reply. Retries of
the same sender/destination/key do not create a second queued or journaled prompt, even
after a queued message was discarded by archiving. Durable receipts retain the accepted key. Different
text under a used key fails. Self-send, read-only targets, permission escalation and relay
chains beyond eight hops are refused. Existing child send/spawn retain their restrictions
and carry the same provenance so they cannot reset relay depth. A human prompt resets it.

### Prompt queue

A prompt sent while a turn runs waits in the daemon's queue for its session.
`SessionHead.queue` lists the waiting prompts in the order they will run, each a
`QueuedPrompt` with its `prompt_id`, `text`, image count, and sender: the user `by`, or the
`agent_message` of another agent's message. A prompt leaves the queue as its turn starts. The
queue arrives with the session list (`Machine::sessions`), which every client gets again
whenever any session's queue changes; a vault lists none.

Whoever may `SendPrompt` to a session may edit its queue, messages from other agents
included:

- `RemoveQueued { session_id, prompt_id }` drops a prompt without running it.
- `MoveQueued { session_id, prompt_id, before }` moves it just before the queued prompt
  `before`, or to the end when `before` is absent.
- `SendQueuedNow { session_id, prompt_id }` runs it next, ahead of the rest, which keep
  their order: it interrupts the running turn (or cancels a retry waiting for a usage limit
  to reset).
- `MergeQueued { session_id, prompt_ids }` merges the listed prompts into one, so they run
  as one turn: it keeps the first one's `prompt_id` and place, joins their texts in the
  listed order with a blank line between them, and carries all their images in that order,
  each prompt's `[Image #N]` markers renumbered to count across the merged prompt. The
  daemon merges them because clients see only an image count. Fewer than two prompts, one
  listed twice, a message from another agent (merging would lose who sent it), prompts
  different users sent, or images over `MAX_PROMPT_IMAGE_BYTES` together are refused with
  `bad_request`. Listing the prompts the client saw, rather than "the whole queue", means a
  prompt queued or started meanwhile is never merged by surprise.

Each answers `Applied`. A prompt that has started is refused with `conflict`, and an unknown
one with `not_found`.

### Account settings (client API 6)

`Account.config_dir` exposes the host-local configuration directory (absent means the
provider default). `send(host, SetAccountSettings { account_id, label, config_dir })`
updates the label and directory for an existing account; only owners may do this.
The daemon persists configuration and broadcasts the refreshed account list. Labels
must be non-empty. Directory changes require all sessions on the daemon archived;
provider and account id cannot be changed. No provider credentials are exposed.
The wire change is additive (protocol 4); the native record shape changes (client API 6).

### Turn limit

`Machine::resources` carries the host's `max_turns`, with its `running_turns` and
`waiting_turns`. `send(host, SetResourceLimits { max_turns })` changes the limit live; only
owners may do this. It must be 1 to `herder_protocol::MAX_TURNS_LIMIT`, else `bad_request`.
Raising it starts waiting turns at once; lowering it stops no running turn. The daemon keeps
the limit in its config and sends every client the new `resources`. The wire change is
additive (protocol 4) and this API is unchanged.

### Daemon settings

`send(host, GetSettings)` answers `Settings`: every daemon-wide setting of the daemon's config
file as a `DaemonSettings` (listen address, log, provider binaries, task, failover, title,
resource and project-discovery settings, and what a vault backs up or keeps), with the
read-only `data_dir` and `is_vault`, and `restart_required` when the file holds settings not
in effect yet. `send(host, SetSettings { settings })` writes the values that changed, in
place, and answers the same way; a value the daemon cannot run with is refused with
`bad_request`, changing nothing. The turn limit applies at once; the rest after
`send(host, RestartDaemon)`, which answers `Applied` and restarts the daemon; clients
reconnect as after any restart. Accounts, projects and the vault link keep their own commands.
Only owners may do any of this. The wire change is additive (protocol 4) and this API is
unchanged.

### Turn usage (client API 10)

`EventBody::TurnCompleted` carries an optional `usage` (`TurnUsage`): the turn's input,
output, cache-read and cache-write tokens, and its cost in US dollars, with `cost_estimated`
set when herder priced the tokens itself rather than taking the provider's figure. It is
absent when the provider reported nothing, and in turns journaled before it existed.
`send(host, GetUsageSummary { period })`, with `period` one of the last 24 hours, 7 days,
30 days or the current UTC calendar month, answers `UsageSummary { period, since, totals }`:
one `UsageTotal` per account and model with a turn on that daemon's host in the period.
Owners and members may ask; each daemon answers for its own host, and a client adds its
machines' answers up. The wire change is additive (protocol 4); the native shape of
`TurnCompleted` and `CommandResult` changes (client API 10).
