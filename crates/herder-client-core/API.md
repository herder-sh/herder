# herder-client-core public API

`CLIENT_API_VERSION = 4`

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
  vault's `VaultStatus` message (`VaultStatus`, `HostReplication`).

## Shape, and how it maps to UniFFI

The API is designed so that the UniFFI layer (P6.2, for Swift and Kotlin) wraps it without
adapting anything:

| Rust                                                          | UniFFI                          |
| ------------------------------------------------------------- | ------------------------------- |
| `Client`, `SessionSubscription`, `TerminalStream`, `Changes`  | objects (`Arc`, `Send + Sync`)  |
| `Machine`, `ConnectionQuality`, `SessionUpdate`, `NewAccount`, `PairingUri` | records          |
| `ConnectionState`, `TerminalEvent`                            | enums with named fields         |
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
- `PairingUri` parses with `FromStr` and formats with `Display`. UniFFI cannot export trait
  impls on records, so the FFI layer exports them as two plain functions.

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
| Machines  | `Client::machines`, `Client::changes`, `Machine::connection`, `quality`, `role` | `Client::pair`, `rename`, `forget`, `suspend`, `wake`, `synced`; `PairingUri`              |
| Sessions  | `Machine::sessions`, `Client::subscribe_session` → `SessionUpdate`; a `UserMessage`'s `attachments` | `send`: `CreateSession` (by account, by provider, or the project's default), `ArchiveSession`, `UnarchiveSession`, `SendPrompt` (with `images`), `GetAttachment` → `CommandResult::Attachment`, `Interrupt`, `SetModel`, `SetPermissionMode`, `ComposeDown` |
| Projects  | `Machine::projects`; a `Project`'s `icon`, the hash of its icon file, to cache it by | anyone: `send`: `GetProjectIcon` → `CommandResult::ProjectIcon` (`not_found` when it has none; fetch again when `icon` changes). Owners: `send`: `ListDirectory` → `CommandResult::Directory`, `AddProject` → `CommandResult::ProjectAdded`, `SetProjectSettings`, `RemoveProject` (refused with `conflict` while it has live sessions; deletes nothing on disk) |
| Approvals | `ApprovalRequested` / `QuestionAsked` / `…Escalated` / `…Resolved` / `QuestionAnswered` events; `SessionHead::children_need_you` | `send`: `AnswerApproval`, `AnswerQuestion`                                               |
| Terminals | `Machine::terminals`; `TerminalStream::next` → `TerminalEvent`       | `Client::open_terminal`, `attach_terminal`; `TerminalStream::input`, `resize`; drop = detach |
| PRs       | `PrLinked` / `PrUpdated` / `PrUnlinked` events                       | `send`: `LinkPr`, `UnlinkPr`                                                              |
| Accounts  | `Machine::accounts`, `failover` (the pin; every account takes part in rotation); `AccountSwitched` / `ProviderSwitched` events | `Client::add_account` with `NewAccount` (a login terminal); `send`: `SwitchAccount`, `SwitchProvider` |
| Fleet     | `Machine::hosts` (a vault), `Machine::vault` (what it holds of each host, live), `SessionHead::host_id`, `Machine::projects`, `resources`, `session_usage` | read-only: a vault rejects commands with `read_only`. To fork any session (its host up or gone) onto a host, `send` `ForkSession` to that host's machine (owners only) → `CommandResult::SessionForked`; the new session joins that machine's list, the original is left as it is |

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
| `async pair(link: String) -> Result<Machine, Error>` | Pairs with the daemon a `herder://pair` link names and saves it. |
| `rename(host_id: HostId, name: String) -> Result<(), Error>` | Shows a machine as `name` on this device. |
| `forget(host_id: HostId) -> Result<(), Error>` | Unpairs a machine on this device. |
| `async synced(host_id: HostId) -> Result<(), Error>` | Waits until a machine is connected and has sent everything owed for what was sent before. |
| `suspend()` | The app went to the background: saves the offline cache (blocking) and stops retrying lost connections. |
| `wake()` | The app is in the foreground: reconnects every disconnected machine now and probes every connected one, replacing a dead connection. |
| `subscribe_session(host_id: HostId, session_id: SessionId) -> Result<SessionSubscription, Error>` | Streams a session, cached state first, across reconnects. |
| `async send(host_id: HostId, command: CommandBody) -> Result<CommandResult, Error>` | Sends a command and waits for the answer; resent with the same id after a reconnect. |
| `async open_terminal(host_id: HostId, session_id: SessionId, cols: u16, rows: u16) -> Result<TerminalStream, Error>` | Opens a shell in a session's worktree; owners only. |
| `async add_account(host_id: HostId, account: NewAccount, cols: u16, rows: u16) -> Result<TerminalStream, Error>` | Runs a provider login in a login terminal; owners only. |
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

- `Machine` — `host_id`, `name`, `addresses`, `fingerprint`, `connection`, `quality`, `role`,
  `sessions`, `hosts`, `projects`, `accounts`, `failover`, `terminals`, `resources`,
  `session_usage`, `vault` (`Option<VaultStatus>`: a vault's totals and per-host replication;
  `None` for a daemon and while not connected).
- `ConnectionQuality` — `connected_since: Option<Timestamp>` (when the current connection
  was established; `None` while not connected), `reconnects: u32` (connections established
  after the first, since the client opened), `last_rtt_ms`, `average_rtt_ms`, `min_rtt_ms`,
  `max_rtt_ms: Option<u32>` (round trips of the last 20 pongs on the current connection, in
  milliseconds; `None` until one came back), `missed_pongs: u32` (pings whose pong did not come
  back before the next ping was due, late or lost, since the client opened).
- `SessionUpdate` — `events: Vec<Event>` (new durable events, in seq order) and
  `streaming: Vec<Item>` (every item streaming now; replaces the previous list).
- `NewAccount` — `account_id`, `provider`, `label: Option<String>`, `config_dir: Option<String>`.
- `PairingUri` — `hosts: Vec<String>`, `fingerprint: String`, `code: String`; `FromStr`
  (fails with `Error::InvalidLink`) and `Display` (`herder://pair?…`).

### Enums

- `ConnectionState` — `Connecting`, `Connected`, `Disconnected { error: String }`.
- `TerminalEvent` — `Output { data: Vec<u8> }`, `Reattached`, `Closed { exit_code: Option<i32> }`.
- `Error` — `InvalidLink { message }`, `Pairing { message }`, `UnknownMachine { host_id }`,
  `Rejected { info: ErrorInfo }` (the daemon refused; `info.code` says why, e.g. `forbidden`,
  `read_only`), `Local { message }`, `Closed`.

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

## Connection quality

A connection is pinged as soon as it is up and every 15 s after. Each ping carries a fresh
payload and only its own pong answers it, so `Machine::quality` holds true round trips: the
latest, and the mean, shortest and longest of the last 20, all cleared when the connection
ends. A ping still unanswered when the next is due counts in `missed_pongs`; the probe a
`wake()` sends counts its round trip too. `connected_since` and `reconnects` say how stable
the connection is. Each pong updates the machine, so `Changes` fires about every 15 s per
connected machine.

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
the same sender/destination/key do not create a second queued or journaled prompt. Different
text under a used key fails. Self-send, read-only targets, permission escalation and relay
chains beyond eight hops are refused. Existing child send/spawn retain their restrictions
and carry the same provenance so they cannot reset relay depth. A human prompt resets it.
