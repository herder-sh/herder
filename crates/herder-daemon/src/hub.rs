//! Fan-out of session events to connected clients.
//!
//! The session manager publishes here through [`EventSink`], always after the append has
//! committed and in seq order per session:
//!
//! - every durable [`Event`] as stored. It reaches every client subscribed to the session, or
//!   the client is disconnected; it is never dropped.
//! - a snapshot when an item starts streaming, then a delta for each piece of text, until the
//!   item's `item_added` event (or the end of its turn) completes it. Deltas are coalesced per
//!   client and flushed every [`FLUSH_INTERVAL`]; a client that falls behind loses its pending
//!   deltas and gets a snapshot of the item once it catches up.
//! - the session list whenever it changes, sent to every client.
//! - the account list whenever an account's usage changes, sent to every client, with the
//!   daemon's failover settings ([`Hub::set_failover`]).
//! - the terminal list whenever it changes, sent to owners only ([`crate::terminal`]).
//! - the project list whenever it changes, sent to every client, and after the other lists
//!   on connect once there is one ([`crate::projects`]).
//! - each session's resource usage whenever it changes, sent to every client, and on connect
//!   for every session with something running ([`crate::resources`]).
//! - the host's resources and turns whenever they change, sent to every client, and the latest
//!   on connect ([`crate::resources::admission`]).
//! - on a vault, the host list whenever a host's liveness changes, and the vault's status
//!   whenever what it holds changes, sent to every client, and the latest of each on connect
//!   ([`crate::vault`]).
//! - the skill library's status and each session's skills whenever they change, sent to every
//!   client, and on connect the latest status and every session's non-empty skills.
//!
//! Terminal output does not pass through the hub: each terminal queues its bytes straight onto
//! its attached clients' outboxes with [`Outbox::terminal_output`].

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use herder_protocol::{
    Account, Event, EventBody, FailoverSettings, FleetHost, HostResources, Item, ItemBody, ItemId,
    Project, ProviderStatus, Role, Seq, ServerMessage, SessionHead, SessionId, SessionSkill,
    SessionUsage, SkillsStatus, Terminal, TerminalId, VaultStatus,
};

use crate::session::EventSink;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tracing::debug;

/// How often pending deltas are flushed to each client.
pub const FLUSH_INTERVAL: Duration = Duration::from_millis(75);

/// Queued messages at which a client's pending deltas are dropped instead of queued; once its
/// queue is shorter again, it gets a snapshot of each item whose deltas it lost.
pub const DELTA_BACKLOG: usize = 16;

/// Queued messages at which a client is disconnected. Durable events are never dropped, so a
/// client this far behind resumes from its cursor on a new connection instead.
pub const MAX_BACKLOG: usize = 1024;

/// Queued messages at which terminal output disconnects a client instead of being queued. The
/// bytes are not stored anywhere else, so a client this far behind reconnects and re-attaches,
/// which replays the terminal's scrollback.
pub const TERMINAL_BACKLOG: usize = 256;

/// Fan-out point between the journal writer and the connected clients.
#[derive(Debug, Default)]
pub struct Hub {
    state: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    /// Items streaming right now, per session, in the order they began.
    items: HashMap<SessionId, Vec<Item>>,
    /// The latest usage of each session with something running.
    usage: HashMap<SessionId, SessionUsage>,
    /// The host's latest resources.
    host: Option<HostResources>,
    /// A vault's latest host list; `None` on a daemon.
    fleet: Option<Vec<FleetHost>>,
    /// A vault's latest status; `None` on a daemon.
    vault: Option<VaultStatus>,
    /// The skill library's latest status; `None` until the daemon publishes one.
    skills: Option<SkillsStatus>,
    /// The skills of each session that has any.
    session_skills: HashMap<SessionId, Vec<SessionSkill>>,
    /// The latest project list; `None` until discovery publishes its first.
    projects: Option<Vec<Project>>,
    /// How sessions fail over, sent with every account list.
    failover: FailoverSettings,
    outboxes: Vec<Arc<Outbox>>,
}

impl EventSink for Hub {
    fn event(&self, event: &Event) {
        self.publish(event);
    }

    fn snapshot(&self, session_id: &SessionId, item: &Item) {
        self.begin_item(session_id, item);
    }

    fn delta(&self, session_id: &SessionId, item_id: &ItemId, text: &str) {
        self.append_delta(session_id, item_id, text);
    }

    fn sessions_changed(&self, sessions: &[SessionHead]) {
        let message = ServerMessage::Sessions {
            sessions: sessions.to_vec(),
        };
        let state = self.lock();
        for outbox in &state.outboxes {
            let mut inner = outbox.lock();
            inner.push(message.clone());
            inner.sessions_sent = true;
            drop(inner);
            outbox.wake();
        }
    }

    fn accounts_changed(&self, accounts: &[Account]) {
        let state = self.lock();
        let message = ServerMessage::Accounts {
            accounts: accounts.to_vec(),
            failover: state.failover.clone(),
        };
        for outbox in &state.outboxes {
            let mut inner = outbox.lock();
            inner.push(message.clone());
            inner.accounts_sent = true;
            drop(inner);
            outbox.wake();
        }
    }
}

impl Hub {
    /// Sends a durable event, just appended to the journal, to the session's subscribers.
    fn publish(&self, event: &Event) {
        let mut state = self.lock();
        let streaming = state.items.entry(event.session_id.clone()).or_default();
        match &event.body {
            EventBody::ItemAdded { item } => streaming.retain(|live| live.id != item.id),
            EventBody::TurnCompleted { turn_id, .. }
            | EventBody::TurnInterrupted { turn_id }
            | EventBody::TurnFailed { turn_id, .. } => {
                streaming.retain(|live| live.turn_id != *turn_id);
            }
            _ => {}
        }
        for outbox in &state.outboxes {
            outbox.lock().event(event, &state.items);
            outbox.wake();
        }
    }

    /// Starts streaming `item`, whose text later deltas extend; replaces an item with its id.
    fn begin_item(&self, session_id: &SessionId, item: &Item) {
        let mut state = self.lock();
        let streaming = state.items.entry(session_id.clone()).or_default();
        match streaming.iter_mut().find(|live| live.id == item.id) {
            Some(live) => *live = item.clone(),
            None => streaming.push(item.clone()),
        }
        for outbox in &state.outboxes {
            outbox.lock().begin_item(session_id, item);
            outbox.wake();
        }
    }

    /// Appends `text` to a streaming item's text, or to a tool result's output.
    fn append_delta(&self, session_id: &SessionId, item_id: &ItemId, text: &str) {
        let mut state = self.lock();
        let Some(item) = state
            .items
            .get_mut(session_id)
            .and_then(|items| items.iter_mut().find(|live| live.id == *item_id))
        else {
            debug!(%session_id, %item_id, "dropping a delta for an item that is not streaming");
            return;
        };
        append(&mut item.body, text);
        for outbox in &state.outboxes {
            outbox.lock().delta(session_id, item_id, text);
        }
    }

    /// Flushes every client's pending deltas each [`FLUSH_INTERVAL`] until `shutdown`.
    pub async fn run_flusher(&self, shutdown: CancellationToken) {
        let mut tick = tokio::time::interval(FLUSH_INTERVAL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                () = shutdown.cancelled() => return,
                _ = tick.tick() => self.flush(),
            }
        }
    }

    pub(crate) fn flush(&self) {
        let state = self.lock();
        for outbox in &state.outboxes {
            outbox.lock().flush(&state.items);
            outbox.wake();
        }
    }

    /// Registers a client that has completed its hello as `role`; from now on it receives every
    /// session list change, and every terminal list change if it is an owner.
    pub(crate) fn connect(&self, outbox: &Arc<Outbox>, role: Role) {
        let mut state = self.lock();
        let mut inner = outbox.lock();
        inner.owner = role == Role::Owner;
        if let Some(host) = &state.host {
            inner.push(ServerMessage::HostResources(host.clone()));
        }
        if let Some(hosts) = &state.fleet {
            inner.push(ServerMessage::Hosts {
                hosts: hosts.clone(),
            });
        }
        if let Some(status) = &state.vault {
            inner.push(ServerMessage::VaultStatus(status.clone()));
        }
        for (session_id, usage) in &state.usage {
            inner.push(ServerMessage::SessionResources {
                session_id: session_id.clone(),
                usage: usage.clone(),
            });
        }
        if let Some(status) = &state.skills {
            inner.push(ServerMessage::SkillsStatus(status.clone()));
        }
        for (session_id, skills) in &state.session_skills {
            inner.push(ServerMessage::SessionSkills {
                session_id: session_id.clone(),
                skills: skills.clone(),
            });
        }
        drop(inner);
        outbox.wake();
        state.outboxes.push(Arc::clone(outbox));
    }

    /// Sends a session's new resource usage to every client. A usage with no processes and no
    /// containers is sent once and then no longer to clients that connect later.
    pub fn session_resources(&self, session_id: &SessionId, usage: SessionUsage) {
        let mut state = self.lock();
        if usage.processes == 0 && usage.containers.is_empty() {
            state.usage.remove(session_id);
        } else {
            state.usage.insert(session_id.clone(), usage.clone());
        }
        let message = ServerMessage::SessionResources {
            session_id: session_id.clone(),
            usage,
        };
        for outbox in &state.outboxes {
            outbox.lock().push(message.clone());
            outbox.wake();
        }
    }

    /// Sends the host's new resources to every client, and to clients that connect later.
    pub fn host_resources(&self, resources: HostResources) {
        let mut state = self.lock();
        state.host = Some(resources.clone());
        let message = ServerMessage::HostResources(resources);
        for outbox in &state.outboxes {
            outbox.lock().push(message.clone());
            outbox.wake();
        }
    }

    /// Sends a vault's new host list to every client, and to clients that connect later.
    pub(crate) fn hosts_changed(&self, hosts: Vec<FleetHost>) {
        let mut state = self.lock();
        let message = ServerMessage::Hosts {
            hosts: hosts.clone(),
        };
        state.fleet = Some(hosts);
        for outbox in &state.outboxes {
            outbox.lock().push(message.clone());
            outbox.wake();
        }
    }

    /// Sends a vault's status to every client, and to clients that connect later, unless it is
    /// the one they have.
    pub(crate) fn vault_status(&self, status: VaultStatus) {
        let mut state = self.lock();
        if state.vault.as_ref() == Some(&status) {
            return;
        }
        let message = ServerMessage::VaultStatus(status.clone());
        state.vault = Some(status);
        for outbox in &state.outboxes {
            outbox.lock().push(message.clone());
            outbox.wake();
        }
    }

    /// Sends the skill library's status to every client, and to clients that connect later.
    pub fn skills_status(&self, status: SkillsStatus) {
        let mut state = self.lock();
        let message = ServerMessage::SkillsStatus(status.clone());
        state.skills = Some(status);
        for outbox in &state.outboxes {
            outbox.lock().push(message.clone());
            outbox.wake();
        }
    }

    /// Sends a session's skills to every client. An empty list is sent once and then no
    /// longer to clients that connect later.
    pub fn session_skills(&self, session_id: &SessionId, skills: Vec<SessionSkill>) {
        let mut state = self.lock();
        if skills.is_empty() {
            state.session_skills.remove(session_id);
        } else {
            state
                .session_skills
                .insert(session_id.clone(), skills.clone());
        }
        let message = ServerMessage::SessionSkills {
            session_id: session_id.clone(),
            skills,
        };
        for outbox in &state.outboxes {
            outbox.lock().push(message.clone());
            outbox.wake();
        }
    }

    /// Sends the new project list to every client, and keeps it for clients that connect later.
    pub(crate) fn projects_changed(&self, projects: Vec<Project>) {
        let mut state = self.lock();
        let message = ServerMessage::Projects {
            projects: projects.clone(),
        };
        state.projects = Some(projects);
        for outbox in &state.outboxes {
            let mut inner = outbox.lock();
            inner.push(message.clone());
            inner.projects_sent = true;
            drop(inner);
            outbox.wake();
        }
    }

    /// The latest project list; empty until discovery published one.
    pub(crate) fn projects(&self) -> Vec<Project> {
        self.lock().projects.clone().unwrap_or_default()
    }

    /// Queues the latest project list to a client after [`Hub::connect`], unless a change
    /// already reached it or discovery has published none yet.
    pub(crate) fn initial_projects(&self, outbox: &Outbox) {
        let state = self.lock();
        let mut inner = outbox.lock();
        if let Some(projects) = &state.projects
            && !inner.projects_sent
        {
            inner.push(ServerMessage::Projects {
                projects: projects.clone(),
            });
        }
        drop(inner);
        outbox.wake();
    }

    /// Sends the new terminal list to every owner.
    pub(crate) fn terminals_changed(&self, terminals: &[Terminal]) {
        self.to_owners(&[ServerMessage::Terminals {
            terminals: terminals.to_vec(),
        }]);
    }

    /// Tells every owner that a terminal's shell exited with `exit_code`, then sends the
    /// remaining `terminals`.
    pub(crate) fn terminal_closed(
        &self,
        terminal_id: &TerminalId,
        exit_code: Option<i32>,
        terminals: &[Terminal],
    ) {
        self.to_owners(&[
            ServerMessage::TerminalClosed {
                terminal_id: terminal_id.clone(),
                exit_code,
            },
            ServerMessage::Terminals {
                terminals: terminals.to_vec(),
            },
        ]);
    }

    /// Queues `messages`, the last of them a terminal list, to every owner.
    fn to_owners(&self, messages: &[ServerMessage]) {
        let state = self.lock();
        for outbox in &state.outboxes {
            let mut inner = outbox.lock();
            if !inner.owner {
                continue;
            }
            for message in messages {
                inner.push(message.clone());
            }
            inner.terminals_sent = true;
            drop(inner);
            outbox.wake();
        }
    }

    /// Queues the terminal list read after [`Hub::connect`] to an owner, unless a change already
    /// reached it, as [`Hub::initial_sessions`] does for sessions.
    pub(crate) fn initial_terminals(&self, outbox: &Outbox, terminals: Vec<Terminal>) {
        let _state = self.lock();
        let mut inner = outbox.lock();
        if inner.owner && !inner.terminals_sent {
            inner.push(ServerMessage::Terminals { terminals });
        }
        drop(inner);
        outbox.wake();
    }

    /// Queues the session list read after [`Hub::connect`], unless a change already reached
    /// the client: that list is at least as new, as every change is announced after it lands.
    pub(crate) fn initial_sessions(&self, outbox: &Outbox, sessions: Vec<SessionHead>) {
        let _state = self.lock();
        let mut inner = outbox.lock();
        if !inner.sessions_sent {
            inner.push(ServerMessage::Sessions { sessions });
        }
        drop(inner);
        outbox.wake();
    }

    /// Sends `failover` with every account list from now on; set once at startup, as the
    /// daemon's failover settings do not change while it runs.
    pub fn set_failover(&self, failover: FailoverSettings) {
        self.lock().failover = failover;
    }

    /// Queues the account list read after [`Hub::connect`], unless a change already reached
    /// the client, as [`Hub::initial_sessions`] does for sessions.
    pub(crate) fn initial_accounts(&self, outbox: &Outbox, accounts: Vec<Account>) {
        let state = self.lock();
        let mut inner = outbox.lock();
        if !inner.accounts_sent {
            let failover = state.failover.clone();
            inner.push(ServerMessage::Accounts { accounts, failover });
        }
        drop(inner);
        outbox.wake();
    }

    /// Queues the provider list read after [`Hub::connect`], unless a change already reached
    /// the client, as [`Hub::initial_accounts`] does for accounts.
    pub(crate) fn initial_providers(&self, outbox: &Outbox, providers: Vec<ProviderStatus>) {
        let _state = self.lock();
        let mut inner = outbox.lock();
        if !inner.providers_sent {
            inner.push(ServerMessage::Providers { providers });
        }
        drop(inner);
        outbox.wake();
    }

    /// Sends the provider list to every client whenever a probe or install changes it.
    pub fn providers_changed(&self, providers: &[ProviderStatus]) {
        let message = ServerMessage::Providers {
            providers: providers.to_vec(),
        };
        let state = self.lock();
        for outbox in &state.outboxes {
            let mut inner = outbox.lock();
            inner.push(message.clone());
            inner.providers_sent = true;
            drop(inner);
            outbox.wake();
        }
    }

    pub(crate) fn disconnect(&self, outbox: &Arc<Outbox>) {
        self.lock()
            .outboxes
            .retain(|other| !Arc::ptr_eq(other, outbox));
    }

    /// Starts a subscription in replay mode: live events are held back until
    /// [`Hub::go_live`].
    pub(crate) fn subscribe(&self, outbox: &Outbox, session_id: &SessionId) {
        let _state = self.lock();
        let mut inner = outbox.lock();
        inner.purge(session_id);
        inner
            .subs
            .insert(session_id.clone(), Sub::Replaying(Vec::new()));
    }

    /// Ends replay after `last_seq`: queues the held live events past it and a snapshot of each
    /// streaming item, then streams live.
    pub(crate) fn go_live(&self, outbox: &Outbox, session_id: &SessionId, last_seq: Seq) {
        let state = self.lock();
        let mut inner = outbox.lock();
        let Some(sub) = inner.subs.get_mut(session_id) else {
            return;
        };
        let held = match std::mem::replace(sub, Sub::Live { last_seq }) {
            Sub::Replaying(held) => held,
            Sub::Live { .. } => Vec::new(),
        };
        for event in &held {
            inner.event(event, &state.items);
        }
        for item in state.items.get(session_id).into_iter().flatten() {
            inner.begin_item(session_id, item);
        }
        drop(inner);
        outbox.wake();
    }

    pub(crate) fn unsubscribe(&self, outbox: &Outbox, session_id: &SessionId) {
        let _state = self.lock();
        let mut inner = outbox.lock();
        inner.subs.remove(session_id);
        inner.purge(session_id);
    }

    /// Longest queue any connected client has had.
    #[cfg(test)]
    pub(crate) fn peak_backlog(&self) -> usize {
        let state = self.lock();
        state
            .outboxes
            .iter()
            .map(|outbox| outbox.lock().peak)
            .max()
            .unwrap_or(0)
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        // A panic while holding the lock leaves the state consistent: every update is a
        // single insert, retain or push.
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Appends a delta to an item's streamed text, the way clients apply it.
pub(crate) fn append(body: &mut ItemBody, delta: &str) {
    match body {
        ItemBody::UserMessage { text, .. }
        | ItemBody::AssistantMessage { text }
        | ItemBody::Reasoning { text } => text.push_str(delta),
        ItemBody::ToolResult { output, .. } => output.push_str(delta),
        ItemBody::ToolCall { .. } | ItemBody::Unknown => {}
    }
}

/// One client's queue of messages waiting to be written, and its subscriptions.
///
/// Lock order: the hub's state, then an outbox.
#[derive(Debug, Default)]
pub(crate) struct Outbox {
    inner: Mutex<Inner>,
    /// Signalled when the queue gains a message or the outbox closes.
    ready: Notify,
    /// Signalled when the queue loses a message.
    drained: Notify,
}

#[derive(Debug, Default)]
struct Inner {
    queue: VecDeque<ServerMessage>,
    subs: HashMap<SessionId, Sub>,
    /// Coalesced deltas not yet queued, one entry per item.
    pending: Vec<Pending>,
    /// Items whose deltas were dropped; each is owed a snapshot.
    stale: Vec<(SessionId, ItemId)>,
    /// Whether a session list change has been queued since the client connected.
    sessions_sent: bool,
    /// Whether an account list change has been queued since the client connected.
    accounts_sent: bool,
    /// Whether a provider list change has been queued since the client connected.
    providers_sent: bool,
    /// Whether the client's user is an owner, and so sees terminals.
    owner: bool,
    /// Whether a terminal list change has been queued since the client connected.
    terminals_sent: bool,
    /// Whether a project list change has been queued since the client connected.
    projects_sent: bool,
    state: OutboxState,
    #[cfg(test)]
    peak: usize,
}

#[derive(Debug)]
enum Sub {
    /// Replay from the journal is running; live events wait here.
    Replaying(Vec<Event>),
    /// Streaming; `last_seq` is the latest event the client has been sent.
    Live { last_seq: Seq },
}

#[derive(Debug)]
struct Pending {
    session_id: SessionId,
    item_id: ItemId,
    text: String,
}

/// Whether an outbox still accepts messages.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum OutboxState {
    #[default]
    Open,
    /// The client stopped; write what is queued, then close.
    Finished,
    /// The client fell [`MAX_BACKLOG`] behind; the queue was discarded.
    Overflowed,
}

impl Outbox {
    /// Queues a message that must not be dropped, such as a command answer.
    pub(crate) fn push(&self, message: ServerMessage) {
        self.lock().push(message);
        self.wake();
    }

    pub(crate) fn push_all(&self, messages: impl IntoIterator<Item = ServerMessage>) {
        let mut inner = self.lock();
        for message in messages {
            inner.push(message);
        }
        drop(inner);
        self.wake();
    }

    /// Queues bytes a terminal wrote, or closes the outbox as [`OutboxState::Overflowed`] when
    /// [`TERMINAL_BACKLOG`] messages are already queued. False once the outbox no longer accepts
    /// messages, so the terminal can let go of it.
    pub(crate) fn terminal_output(&self, message: ServerMessage) -> bool {
        let mut inner = self.lock();
        if inner.queue.len() >= TERMINAL_BACKLOG {
            inner.overflow();
        } else {
            inner.push(message);
        }
        let open = inner.state == OutboxState::Open;
        drop(inner);
        self.wake();
        open
    }

    /// The next message to write.
    pub(crate) fn pop(&self) -> Option<ServerMessage> {
        let message = self.lock().queue.pop_front();
        if message.is_some() {
            self.drained.notify_one();
        }
        message
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.lock().queue.is_empty()
    }

    pub(crate) fn state(&self) -> OutboxState {
        self.lock().state
    }

    /// Stops accepting messages; what is already queued is still written.
    pub(crate) fn finish(&self) {
        let mut inner = self.lock();
        if inner.state == OutboxState::Open {
            inner.state = OutboxState::Finished;
        }
        drop(inner);
        self.wake();
    }

    /// Waits until a message is queued or the outbox stops accepting messages.
    pub(crate) async fn ready(&self) {
        self.ready.notified().await;
    }

    /// Waits until fewer than `len` messages are queued, or the outbox is no longer open.
    pub(crate) async fn wait_below(&self, len: usize) {
        loop {
            {
                let inner = self.lock();
                if inner.queue.len() < len || inner.state != OutboxState::Open {
                    return;
                }
            }
            self.drained.notified().await;
        }
    }

    fn wake(&self) {
        // A single task waits on each: the connection's writer on `ready`, its reader on
        // `drained`. `notify_one` keeps a permit when nobody waits yet, so no wake-up is lost.
        self.ready.notify_one();
        self.drained.notify_one();
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // Every update leaves the outbox consistent, so a poisoned lock is still usable.
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl Inner {
    fn push(&mut self, message: ServerMessage) {
        if self.state != OutboxState::Open {
            return;
        }
        if self.queue.len() >= MAX_BACKLOG {
            self.overflow();
            return;
        }
        self.queue.push_back(message);
        #[cfg(test)]
        {
            self.peak = self.peak.max(self.queue.len());
        }
    }

    fn overflow(&mut self) {
        self.state = OutboxState::Overflowed;
        self.queue.clear();
        self.subs.clear();
        self.pending.clear();
        self.stale.clear();
    }

    fn event(&mut self, event: &Event, items: &HashMap<SessionId, Vec<Item>>) {
        match self.subs.get_mut(&event.session_id) {
            None => {}
            Some(Sub::Replaying(held)) => {
                if held.len() >= MAX_BACKLOG {
                    self.overflow();
                } else {
                    held.push(event.clone());
                }
            }
            // An event is published after its append commits, so replay may already have
            // read it: the journal decides, and anything the client has is skipped.
            Some(Sub::Live { last_seq }) if event.seq <= *last_seq => {}
            Some(Sub::Live { last_seq }) => {
                *last_seq = event.seq;
                if let EventBody::ItemAdded { item } = &event.body {
                    // The event carries the final item; its pending deltas are moot.
                    let done = |session: &SessionId, id: &ItemId| {
                        *session == event.session_id && *id == item.id
                    };
                    self.pending
                        .retain(|pending| !done(&pending.session_id, &pending.item_id));
                    self.stale.retain(|(session, id)| !done(session, id));
                }
                // Deltas queued earlier must reach the client before the event.
                self.flush(items);
                self.push(ServerMessage::Event(event.clone()));
            }
        }
    }

    fn begin_item(&mut self, session_id: &SessionId, item: &Item) {
        if !matches!(self.subs.get(session_id), Some(Sub::Live { .. })) {
            return;
        }
        self.pending
            .retain(|pending| !(pending.session_id == *session_id && pending.item_id == item.id));
        if self.queue.len() >= DELTA_BACKLOG {
            self.mark_stale(session_id, &item.id);
        } else {
            self.stale
                .retain(|(session, id)| !(session == session_id && *id == item.id));
            self.push(ServerMessage::Snapshot {
                session_id: session_id.clone(),
                item: item.clone(),
            });
        }
    }

    fn delta(&mut self, session_id: &SessionId, item_id: &ItemId, text: &str) {
        if !matches!(self.subs.get(session_id), Some(Sub::Live { .. }))
            || self
                .stale
                .iter()
                .any(|(session, id)| session == session_id && id == item_id)
        {
            return;
        }
        match self
            .pending
            .iter_mut()
            .find(|pending| pending.session_id == *session_id && pending.item_id == *item_id)
        {
            Some(pending) => pending.text.push_str(text),
            None => self.pending.push(Pending {
                session_id: session_id.clone(),
                item_id: item_id.clone(),
                text: text.to_owned(),
            }),
        }
    }

    /// Queues owed snapshots and pending deltas, or drops the deltas when the client is behind.
    fn flush(&mut self, items: &HashMap<SessionId, Vec<Item>>) {
        if self.pending.is_empty() && self.stale.is_empty() {
            return;
        }
        if self.queue.len() >= DELTA_BACKLOG {
            for pending in std::mem::take(&mut self.pending) {
                self.mark_stale(&pending.session_id, &pending.item_id);
            }
            return;
        }
        for (session_id, item_id) in std::mem::take(&mut self.stale) {
            let item = items
                .get(&session_id)
                .and_then(|items| items.iter().find(|item| item.id == item_id));
            if let Some(item) = item {
                let item = item.clone();
                self.push(ServerMessage::Snapshot { session_id, item });
            }
        }
        for pending in std::mem::take(&mut self.pending) {
            self.push(ServerMessage::Delta {
                session_id: pending.session_id,
                item_id: pending.item_id,
                text: pending.text,
            });
        }
    }

    fn mark_stale(&mut self, session_id: &SessionId, item_id: &ItemId) {
        if !self
            .stale
            .iter()
            .any(|(session, id)| session == session_id && id == item_id)
        {
            self.stale.push((session_id.clone(), item_id.clone()));
        }
    }

    /// Forgets everything queued or owed for a session that is being (re)subscribed or left.
    fn purge(&mut self, session_id: &SessionId) {
        self.queue.retain(|message| match message {
            ServerMessage::Event(event) => event.session_id != *session_id,
            ServerMessage::Snapshot { session_id: s, .. }
            | ServerMessage::Delta { session_id: s, .. } => s != session_id,
            _ => true,
        });
        self.pending
            .retain(|pending| pending.session_id != *session_id);
        self.stale.retain(|(session, _)| session != session_id);
    }
}

#[cfg(test)]
mod tests {
    use herder_protocol::{TerminalPurpose, TurnId};

    use super::*;

    fn session() -> SessionId {
        SessionId::new("s1")
    }

    fn event(seq: Seq, body: EventBody) -> Event {
        Event {
            session_id: session(),
            seq,
            at: jiff::Timestamp::UNIX_EPOCH,
            by: None,
            body,
        }
    }

    fn message(id: &str, text: &str) -> Item {
        Item {
            agent_message: None,
            follow_up: None,
            parent_call_id: None,
            id: ItemId::new(id),
            turn_id: TurnId::new("t1"),
            body: ItemBody::AssistantMessage { text: text.into() },
        }
    }

    fn live_hub() -> (Hub, Arc<Outbox>) {
        let hub = Hub::default();
        let outbox = Arc::new(Outbox::default());
        hub.connect(&outbox, Role::Owner);
        hub.subscribe(&outbox, &session());
        hub.go_live(&outbox, &session(), 0);
        (hub, outbox)
    }

    fn drain(outbox: &Outbox) -> Vec<ServerMessage> {
        std::iter::from_fn(|| outbox.pop()).collect()
    }

    #[test]
    fn deltas_for_one_item_coalesce_into_one_message_per_flush() {
        let (hub, outbox) = live_hub();
        hub.snapshot(&session(), &message("i1", ""));
        for n in 0..100 {
            EventSink::delta(&hub, &session(), &ItemId::new("i1"), &n.to_string());
        }
        hub.flush();
        let expected: String = (0..100).map(|n| n.to_string()).collect();
        let queued = drain(&outbox);
        assert_eq!(queued.len(), 2, "{queued:?}");
        assert!(matches!(&queued[0], ServerMessage::Snapshot { .. }));
        assert_eq!(
            queued[1],
            ServerMessage::Delta {
                session_id: session(),
                item_id: ItemId::new("i1"),
                text: expected,
            }
        );
    }

    #[test]
    fn pending_deltas_are_queued_before_a_later_event() {
        let (hub, outbox) = live_hub();
        hub.snapshot(&session(), &message("i1", ""));
        EventSink::delta(&hub, &session(), &ItemId::new("i1"), "a");
        hub.event(&event(
            1,
            EventBody::ItemAdded {
                item: message("i2", "x"),
            },
        ));
        let queued = drain(&outbox);
        assert!(matches!(&queued[1], ServerMessage::Delta { text, .. } if text == "a"));
        assert!(matches!(&queued[2], ServerMessage::Event(e) if e.seq == 1));
    }

    #[test]
    fn a_client_behind_loses_deltas_and_gets_a_snapshot_when_it_catches_up() {
        let (hub, outbox) = live_hub();
        hub.snapshot(&session(), &message("i1", ""));
        for seq in 1..=DELTA_BACKLOG as Seq {
            hub.event(&event(seq, EventBody::ModelSwitched { model: "m".into() }));
        }
        EventSink::delta(&hub, &session(), &ItemId::new("i1"), "lost");
        hub.flush();
        EventSink::delta(&hub, &session(), &ItemId::new("i1"), " and ignored");
        hub.flush();
        let queued = drain(&outbox);
        assert!(
            !queued
                .iter()
                .any(|m| matches!(m, ServerMessage::Delta { .. })),
            "{queued:?}"
        );
        EventSink::delta(&hub, &session(), &ItemId::new("i1"), " still");
        hub.flush();
        let queued = drain(&outbox);
        assert_eq!(
            queued,
            [ServerMessage::Snapshot {
                session_id: session(),
                item: message("i1", "lost and ignored still"),
            }]
        );
    }

    #[test]
    fn a_client_too_far_behind_on_durable_events_is_closed() {
        let (hub, outbox) = live_hub();
        for seq in 1..=MAX_BACKLOG as Seq + 1 {
            hub.event(&event(seq, EventBody::ModelSwitched { model: "m".into() }));
        }
        assert_eq!(outbox.state(), OutboxState::Overflowed);
        assert!(outbox.pop().is_none());
    }

    #[test]
    fn events_published_during_replay_are_held_then_deduplicated() {
        let hub = Hub::default();
        let outbox = Arc::new(Outbox::default());
        hub.connect(&outbox, Role::Owner);
        hub.subscribe(&outbox, &session());
        hub.event(&event(2, EventBody::ModelSwitched { model: "a".into() }));
        hub.event(&event(3, EventBody::ModelSwitched { model: "b".into() }));
        assert!(drain(&outbox).is_empty());
        // Replay read the journal through seq 2.
        hub.go_live(&outbox, &session(), 2);
        let seqs: Vec<Seq> = drain(&outbox)
            .into_iter()
            .map(|m| match m {
                ServerMessage::Event(e) => e.seq,
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(seqs, [3]);
    }

    #[test]
    fn an_event_replay_already_read_is_skipped_when_published_late() {
        let hub = Hub::default();
        let outbox = Arc::new(Outbox::default());
        hub.connect(&outbox, Role::Owner);
        hub.subscribe(&outbox, &session());
        // Replay read seq 1 and 2 before seq 2's publish reached the hub.
        hub.go_live(&outbox, &session(), 2);
        hub.event(&event(2, EventBody::ModelSwitched { model: "a".into() }));
        hub.event(&event(3, EventBody::ModelSwitched { model: "b".into() }));
        let queued = drain(&outbox);
        assert!(
            matches!(queued.as_slice(), [ServerMessage::Event(e)] if e.seq == 3),
            "{queued:?}"
        );
    }

    #[test]
    fn the_initial_session_list_never_overtakes_a_change() {
        let hub = Hub::default();
        let heads = |seq| {
            vec![SessionHead {
                session_id: session(),
                host_id: None,
                head_seq: seq,
                status: herder_protocol::SessionStatus::Idle,
                parent: None,
                parent_host: None,
                task: None,
                title: None,
                project_id: None,
                account_id: herder_protocol::AccountId::new("claude"),
                children_need_you: 0,
                queue: Vec::new(),
                chat: false,
            }]
        };
        let first = Arc::new(Outbox::default());
        hub.connect(&first, Role::Owner);
        hub.initial_sessions(&first, heads(1));
        let second = Arc::new(Outbox::default());
        hub.connect(&second, Role::Owner);
        hub.sessions_changed(&heads(2));
        // Read before the change landed, queued after it.
        hub.initial_sessions(&second, heads(1));
        let sessions = ServerMessage::Sessions { sessions: heads(2) };
        assert_eq!(drain(&second), std::slice::from_ref(&sessions));
        assert_eq!(drain(&first)[1], sessions);
    }

    #[test]
    fn session_usage_reaches_every_client_and_later_ones_until_it_is_zero() {
        let hub = Hub::default();
        let usage = |processes| SessionUsage {
            cpu_percent: 1.5,
            memory_bytes: 1024,
            processes,
            containers: Vec::new(),
        };
        let message = |processes| ServerMessage::SessionResources {
            session_id: session(),
            usage: usage(processes),
        };
        let member = Arc::new(Outbox::default());
        hub.connect(&member, Role::Member);
        hub.session_resources(&session(), usage(3));
        assert_eq!(drain(&member), [message(3)]);

        let later = Arc::new(Outbox::default());
        hub.connect(&later, Role::Member);
        assert_eq!(drain(&later), [message(3)]);

        hub.session_resources(&session(), usage(0));
        assert_eq!(drain(&member), [message(0)]);
        assert_eq!(drain(&later), [message(0)]);
        let last = Arc::new(Outbox::default());
        hub.connect(&last, Role::Member);
        assert!(drain(&last).is_empty());
    }

    #[test]
    fn skills_reach_every_client_and_later_ones_until_a_session_has_none() {
        let hub = Hub::default();
        let status = SkillsStatus {
            repo: Some("https://github.com/you/herder-skills".into()),
            head: None,
            last_pull: None,
            pull_error: None,
            skills: Vec::new(),
            reload: Vec::new(),
            accounts: Vec::new(),
        };
        let skills = vec![SessionSkill {
            name: "deploy".into(),
            description: "Deploys.".into(),
            source: herder_protocol::SkillSource::Project,
            path: Some(".claude/skills/deploy".into()),
        }];
        let listed = |skills| ServerMessage::SessionSkills {
            session_id: session(),
            skills,
        };
        let member = Arc::new(Outbox::default());
        hub.connect(&member, Role::Member);
        hub.skills_status(status.clone());
        hub.session_skills(&session(), skills.clone());
        let both = [
            ServerMessage::SkillsStatus(status.clone()),
            listed(skills.clone()),
        ];
        assert_eq!(drain(&member), both);

        let later = Arc::new(Outbox::default());
        hub.connect(&later, Role::Member);
        assert_eq!(drain(&later), both);

        hub.session_skills(&session(), Vec::new());
        assert_eq!(drain(&member), [listed(Vec::new())]);
        let last = Arc::new(Outbox::default());
        hub.connect(&last, Role::Member);
        assert_eq!(drain(&last), [ServerMessage::SkillsStatus(status)]);
    }

    #[test]
    fn account_usage_reaches_every_client_and_never_goes_back() {
        let hub = Hub::default();
        let accounts = |used_percent| {
            vec![Account {
                config_dir: None,
                account_id: herder_protocol::AccountId::new("claude"),
                provider: herder_protocol::Provider::Claude,
                label: "Main".into(),
                email: None,
                usage: vec![herder_protocol::UsageWindow {
                    window: "five_hour".into(),
                    used_percent,
                    resets_at: None,
                }],
                fallback: false,
            }]
        };
        let failover = FailoverSettings { pin: true };
        hub.set_failover(failover.clone());
        let owner = Arc::new(Outbox::default());
        let member = Arc::new(Outbox::default());
        hub.connect(&owner, Role::Owner);
        hub.connect(&member, Role::Member);
        hub.initial_accounts(&owner, accounts(1.0));
        hub.accounts_changed(&accounts(2.0));
        // Read before the change landed, queued after it.
        hub.initial_accounts(&member, accounts(1.0));
        let changed = ServerMessage::Accounts {
            accounts: accounts(2.0),
            failover: failover.clone(),
        };
        assert_eq!(drain(&member), std::slice::from_ref(&changed));
        assert_eq!(
            drain(&owner),
            [
                ServerMessage::Accounts {
                    accounts: accounts(1.0),
                    failover,
                },
                changed
            ]
        );
    }

    #[test]
    fn host_resources_reach_every_client_and_the_latest_later_ones() {
        let hub = Hub::default();
        let host = |running_turns| HostResources {
            cpu_cores: 4,
            cpu_percent: 12.5,
            load_1m: 1.0,
            memory_total_bytes: 8 << 30,
            memory_available_bytes: 4 << 30,
            pressure: None,
            running_turns,
            max_turns: 1,
            waiting_turns: 0,
            constraint: None,
        };
        let member = Arc::new(Outbox::default());
        hub.connect(&member, Role::Member);
        hub.host_resources(host(0));
        hub.host_resources(host(1));
        let message = |running| ServerMessage::HostResources(host(running));
        assert_eq!(drain(&member), [message(0), message(1)]);

        let later = Arc::new(Outbox::default());
        hub.connect(&later, Role::Member);
        assert_eq!(drain(&later), [message(1)]);
    }

    #[test]
    fn project_lists_reach_every_client_and_later_ones_once_published() {
        let hub = Hub::default();
        let before = Arc::new(Outbox::default());
        hub.connect(&before, Role::Member);
        hub.initial_projects(&before);
        assert!(
            drain(&before).is_empty(),
            "no list before discovery publishes one"
        );
        let projects = vec![Project {
            project_id: herder_protocol::ProjectId::new("github.com/org/repo"),
            name: "repo".to_owned(),
            paths: vec!["/src/repo".to_owned()],
            remote: Some("git@github.com:org/repo.git".to_owned()),
            default_permission_mode: None,
            default_account: None,
            setup_command: None,
            icon: None,
            icon_uploaded: false,
            icon_background: None,
        }];
        hub.projects_changed(projects.clone());
        let message = ServerMessage::Projects { projects };
        assert_eq!(drain(&before), std::slice::from_ref(&message));
        let later = Arc::new(Outbox::default());
        hub.connect(&later, Role::Member);
        assert!(drain(&later).is_empty());
        hub.initial_projects(&later);
        assert_eq!(drain(&later), std::slice::from_ref(&message));
        // A list that already reached a client is not sent again.
        hub.initial_projects(&before);
        assert!(drain(&before).is_empty());
    }

    #[test]
    fn terminal_lists_reach_owners_only() {
        let hub = Hub::default();
        let owner = Arc::new(Outbox::default());
        let member = Arc::new(Outbox::default());
        hub.connect(&owner, Role::Owner);
        hub.connect(&member, Role::Member);
        let terminals = vec![Terminal {
            terminal_id: TerminalId::new("t1"),
            purpose: TerminalPurpose::Shell {
                session_id: session(),
            },
        }];
        hub.terminals_changed(&terminals);
        hub.initial_terminals(&member, Vec::new());
        // Read before the change landed, queued after it.
        hub.initial_terminals(&owner, Vec::new());
        assert_eq!(drain(&owner), [ServerMessage::Terminals { terminals }]);
        assert!(drain(&member).is_empty());

        hub.terminal_closed(&TerminalId::new("t1"), Some(1), &[]);
        assert_eq!(
            drain(&owner),
            [
                ServerMessage::TerminalClosed {
                    terminal_id: TerminalId::new("t1"),
                    exit_code: Some(1),
                },
                ServerMessage::Terminals {
                    terminals: Vec::new()
                },
            ]
        );
        assert!(drain(&member).is_empty());
    }

    #[test]
    fn terminal_output_closes_a_client_that_falls_behind() {
        let outbox = Outbox::default();
        let output = || ServerMessage::TerminalOutput {
            terminal_id: herder_protocol::TerminalId::new("t1"),
            data: herder_protocol::Bytes(b"x".to_vec()),
        };
        for _ in 0..TERMINAL_BACKLOG {
            assert!(outbox.terminal_output(output()));
        }
        assert!(!outbox.terminal_output(output()));
        assert_eq!(outbox.state(), OutboxState::Overflowed);
        assert!(outbox.pop().is_none());
    }
}
