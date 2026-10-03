//! What the client holds of one session: its durable events and the items streaming now.
//!
//! Held in memory; the offline cache ([`crate::offline`]) saves the events of recent sessions
//! and seeds a log with them when the client opens. The supervisor resumes from
//! [`SessionLog::last_seq`] either way.

use herder_protocol::{Event, EventBody, Item, ItemBody, ItemId, Seq};

/// One session's events, deduplicated by seq, and its streaming items.
#[derive(Debug, Default)]
pub(crate) struct SessionLog {
    /// Every event received, in increasing seq order.
    events: Vec<Event>,
    /// Items streaming now, in the order they began.
    streaming: Vec<Item>,
}

impl SessionLog {
    /// A log holding `events`, as saved by the offline cache; nothing is streaming.
    pub(crate) fn from_events(events: Vec<Event>) -> Self {
        let mut log = Self::default();
        for event in events {
            log.event(event);
        }
        log
    }

    /// Every event held, oldest first.
    pub(crate) fn events(&self) -> &[Event] {
        &self.events
    }

    /// Seq of the latest event held; 0 for none. The resume cursor.
    pub(crate) fn last_seq(&self) -> Seq {
        self.events.last().map_or(0, |event| event.seq)
    }

    /// Adds a durable event; `false` for one already held, which changes nothing.
    ///
    /// A seq may be skipped: a daemon does not send events its own build cannot encode.
    pub(crate) fn event(&mut self, event: Event) -> bool {
        if event.seq <= self.last_seq() {
            return false;
        }
        match &event.body {
            EventBody::ItemAdded { item } => self.streaming.retain(|live| live.id != item.id),
            EventBody::TurnCompleted { turn_id }
            | EventBody::TurnInterrupted { turn_id }
            | EventBody::TurnFailed { turn_id, .. } => {
                self.streaming.retain(|live| live.turn_id != *turn_id);
            }
            _ => {}
        }
        self.events.push(event);
        true
    }

    /// Sets the full state of a streaming item, replacing what was held for it.
    pub(crate) fn snapshot(&mut self, item: Item) {
        match self.streaming.iter_mut().find(|live| live.id == item.id) {
            Some(live) => *live = item,
            None => self.streaming.push(item),
        }
    }

    /// Appends text to a streaming item; `false` when no snapshot introduced it.
    pub(crate) fn delta(&mut self, item_id: &ItemId, text: &str) -> bool {
        let Some(item) = self.streaming.iter_mut().find(|live| live.id == *item_id) else {
            return false;
        };
        match &mut item.body {
            ItemBody::UserMessage { text: so_far, .. }
            | ItemBody::AssistantMessage { text: so_far }
            | ItemBody::Reasoning { text: so_far } => so_far.push_str(text),
            ItemBody::ToolResult { output, .. } => output.push_str(text),
            ItemBody::ToolCall { .. } | ItemBody::Unknown => {}
        }
        true
    }

    /// Every event after `seq`, oldest first.
    pub(crate) fn events_after(&self, seq: Seq) -> Vec<Event> {
        let start = self.events.partition_point(|event| event.seq <= seq);
        self.events[start..].to_vec()
    }

    /// The items streaming now.
    pub(crate) fn streaming(&self) -> Vec<Item> {
        self.streaming.clone()
    }
}

#[cfg(test)]
mod tests {
    use herder_protocol::{SessionId, Timestamp, TurnId};

    use super::*;

    fn event(seq: Seq, body: EventBody) -> Event {
        Event {
            session_id: SessionId::new("s"),
            seq,
            at: Timestamp::UNIX_EPOCH,
            by: None,
            body,
        }
    }

    fn item(id: &str, turn: &str, text: &str) -> Item {
        Item {
            agent_message: None,
            parent_call_id: None,
            id: ItemId::new(id),
            turn_id: TurnId::new(turn),
            body: ItemBody::AssistantMessage { text: text.into() },
        }
    }

    fn started(turn: &str) -> EventBody {
        EventBody::TurnStarted {
            turn_id: TurnId::new(turn),
        }
    }

    #[test]
    fn events_are_deduplicated_by_seq() {
        let mut log = SessionLog::default();
        assert!(log.event(event(1, started("t1"))));
        assert!(log.event(event(2, started("t2"))));
        assert!(!log.event(event(2, started("t2"))));
        assert!(!log.event(event(1, started("t1"))));
        // A skipped seq is accepted: the daemon may hold events it cannot send.
        assert!(log.event(event(4, started("t4"))));
        assert_eq!(log.last_seq(), 4);
        let seqs: Vec<Seq> = log.events_after(1).iter().map(|e| e.seq).collect();
        assert_eq!(seqs, [2, 4]);
        assert!(log.events_after(4).is_empty());
    }

    #[test]
    fn deltas_extend_snapshots_until_the_item_or_its_turn_completes() {
        let mut log = SessionLog::default();
        assert!(!log.delta(&ItemId::new("i1"), "lost"));
        log.snapshot(item("i1", "t1", "He"));
        assert!(log.delta(&ItemId::new("i1"), "llo"));
        log.snapshot(item("i2", "t1", "x"));
        assert_eq!(
            log.streaming(),
            [item("i1", "t1", "Hello"), item("i2", "t1", "x")]
        );

        // A later snapshot replaces what deltas built.
        log.snapshot(item("i1", "t1", "Hello, world"));
        log.event(event(
            1,
            EventBody::ItemAdded {
                item: item("i2", "t1", "x!"),
            },
        ));
        assert_eq!(log.streaming(), [item("i1", "t1", "Hello, world")]);
        log.event(event(
            2,
            EventBody::TurnFailed {
                turn_id: TurnId::new("t1"),
                error: herder_protocol::TurnError {
                    class: herder_protocol::ErrorClass::Transient,
                    message: "gone".into(),
                },
            },
        ));
        assert!(log.streaming().is_empty());
    }
    #[test]
    fn ancestry_survives_snapshots_completion_and_cached_reconnect() {
        let mut log = SessionLog::default();
        let mut child = item("child", "t1", "Hi");
        child.parent_call_id = Some(ItemId::new("agent-call"));
        log.snapshot(child.clone());
        assert!(log.delta(&child.id, " there"));
        child.body = ItemBody::AssistantMessage {
            text: "Hi there".into(),
        };
        assert_eq!(log.streaming(), [child.clone()]);
        log.event(event(
            1,
            EventBody::ItemAdded {
                item: child.clone(),
            },
        ));
        assert!(log.streaming().is_empty());
        let saved = serde_json::to_vec(log.events()).unwrap();
        let mut reconnected = SessionLog::from_events(serde_json::from_slice(&saved).unwrap());
        assert!(!reconnected.event(event(1, EventBody::ItemAdded { item: child })));
        assert_eq!(reconnected.events(), log.events());
        assert_eq!(reconnected.last_seq(), 1);
    }
}
