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

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use herder_protocol::{
    Event, EventBody, Item, ItemBody, ItemId, Seq, ServerMessage, SessionHead, SessionId,
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

/// Fan-out point between the journal writer and the connected clients.
#[derive(Debug, Default)]
pub struct Hub {
    state: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    /// Items streaming right now, per session, in the order they began.
    items: HashMap<SessionId, Vec<Item>>,
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
}

impl Hub {
    /// Sends a durable event, just appended to the journal, to the session's subscribers.
    fn publish(&self, event: &Event) {
        let mut state = self.lock();
        let streaming = state.items.entry(event.session_id.clone()).or_default();
        match &event.body {
            EventBody::ItemAdded { item } => streaming.retain(|live| live.id != item.id),
            EventBody::TurnCompleted { turn_id }
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

    /// Registers a client that has completed its hello; from now on it receives every
    /// session list change.
    pub(crate) fn connect(&self, outbox: &Arc<Outbox>) {
        self.lock().outboxes.push(Arc::clone(outbox));
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
        ItemBody::UserMessage { text }
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
    use herder_protocol::TurnId;

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
            id: ItemId::new(id),
            turn_id: TurnId::new("t1"),
            body: ItemBody::AssistantMessage { text: text.into() },
        }
    }

    fn live_hub() -> (Hub, Arc<Outbox>) {
        let hub = Hub::default();
        let outbox = Arc::new(Outbox::default());
        hub.connect(&outbox);
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
        hub.connect(&outbox);
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
        hub.connect(&outbox);
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
                head_seq: seq,
            }]
        };
        let first = Arc::new(Outbox::default());
        hub.connect(&first);
        hub.initial_sessions(&first, heads(1));
        let second = Arc::new(Outbox::default());
        hub.connect(&second);
        hub.sessions_changed(&heads(2));
        // Read before the change landed, queued after it.
        hub.initial_sessions(&second, heads(1));
        let sessions = ServerMessage::Sessions { sessions: heads(2) };
        assert_eq!(drain(&second), std::slice::from_ref(&sessions));
        assert_eq!(drain(&first)[1], sessions);
    }
}
