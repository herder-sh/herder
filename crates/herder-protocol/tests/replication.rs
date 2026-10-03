//! Reference in-memory implementation of both ends of the replication protocol, and the
//! behaviour the real host (P3.3) and vault (P3.2) must match: killing the vault mid-stream
//! loses nothing and duplicates nothing, re-sends are idempotent, and the vault never writes
//! to a host's sessions.
//!
//! Every message crosses the simulated connection as JSON text, as on the wire.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use herder_protocol::*;
use serde_json::json;

fn at() -> Timestamp {
    "2026-10-02T12:00:00Z".parse().unwrap()
}

/// The host end: owns its journals, replicates them over one connection at a time.
struct Host {
    host_id: HostId,
    batch_size: usize,
    journals: BTreeMap<SessionId, Vec<JournalRecord>>,
    link: Option<HostLink>,
}

/// What a host knows about one connection; dropped with it.
#[derive(Default)]
struct HostLink {
    /// Last seq sent per session; set from the vault's hello.
    sent: Option<BTreeMap<SessionId, Seq>>,
    /// Last seq the vault acknowledged per session.
    acked: BTreeMap<SessionId, Seq>,
    /// Sessions the vault refused as conflicting.
    stopped: BTreeSet<SessionId>,
}

impl Host {
    fn new(batch_size: usize) -> Self {
        Self {
            host_id: HostId::new("01J9HOST"),
            batch_size,
            journals: BTreeMap::new(),
            link: None,
        }
    }

    /// Appends a local event, as the daemon's store would.
    fn append(&mut self, session: &str, body: serde_json::Value) {
        let journal = self.journals.entry(SessionId::new(session)).or_default();
        journal.push(JournalRecord {
            seq: journal.len() as Seq + 1,
            at: at(),
            by: None,
            body: RawEventBody::from_value(body).unwrap(),
        });
    }

    fn summary(&self, session_id: &SessionId) -> SessionSummary {
        SessionSummary {
            session_id: session_id.clone(),
            project_id: ProjectId::new("github.com/herder-sh/herder"),
            repo: "/home/dev/herder".into(),
            branch: format!("herder/{session_id}"),
            status: SessionStatus::Running,
            prs: Vec::new(),
            parent: None,
            task: None,
            title: None,
            head_seq: self.journals[session_id].len() as Seq,
            updated_at: at(),
        }
    }

    fn connect(&mut self) -> HostMessage {
        self.link = Some(HostLink::default());
        HostMessage::Hello(HostHello {
            replication_version: REPLICATION_VERSION,
            host_id: self.host_id.clone(),
            host_name: "devbox".into(),
            build: "herder/0.0.0".into(),
            pairing_code: None,
        })
    }

    fn receive(&mut self, message: VaultMessage) -> Vec<HostMessage> {
        let Some(link) = &mut self.link else {
            return Vec::new();
        };
        match message {
            VaultMessage::Hello(hello) => {
                if hello.replication_version != REPLICATION_VERSION {
                    self.link = None;
                    return Vec::new();
                }
                let acked: BTreeMap<_, _> = hello
                    .acked
                    .into_iter()
                    .map(|c| (c.session_id, c.after_seq))
                    .collect();
                link.sent = Some(acked.clone());
                link.acked = acked;
                let mut out: Vec<_> = self
                    .journals
                    .keys()
                    .map(|id| HostMessage::Session(self.summary(id)))
                    .collect();
                out.extend(self.pump());
                out
            }
            VaultMessage::Ack(cursor) => {
                let acked = link.acked.entry(cursor.session_id).or_default();
                *acked = (*acked).max(cursor.after_seq);
                Vec::new()
            }
            VaultMessage::Rejected { cursor, reason } => {
                match reason {
                    RejectReason::Gap => {
                        if let Some(sent) = &mut link.sent {
                            sent.insert(cursor.session_id, cursor.after_seq);
                        }
                    }
                    RejectReason::Conflict => {
                        link.stopped.insert(cursor.session_id);
                    }
                }
                self.pump()
            }
            VaultMessage::Error { .. } => {
                self.link = None;
                Vec::new()
            }
            VaultMessage::Unknown => Vec::new(),
        }
    }

    /// Batches of every event not yet sent on this connection.
    fn pump(&mut self) -> Vec<HostMessage> {
        let Some(HostLink {
            sent: Some(sent),
            stopped,
            ..
        }) = &mut self.link
        else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for (session_id, journal) in &self.journals {
            if stopped.contains(session_id) {
                continue;
            }
            let from = sent.get(session_id).copied().unwrap_or(0) as usize;
            for chunk in journal[from.min(journal.len())..].chunks(self.batch_size) {
                out.push(HostMessage::Batch(Batch {
                    session_id: session_id.clone(),
                    events: chunk.to_vec(),
                }));
            }
            sent.insert(session_id.clone(), journal.len() as Seq);
        }
        out
    }
}

/// The vault end: durable journals per host, which survive a kill; per-connection state does not.
#[derive(Default)]
struct Vault {
    paired: BTreeSet<HostId>,
    journals: BTreeMap<(HostId, SessionId), Vec<JournalRecord>>,
    summaries: BTreeMap<(HostId, SessionId), SessionSummary>,
    /// Every event ever written, to prove nothing was written twice.
    writes: usize,
}

/// What a vault knows about one connection: the host its hello authenticated, if any.
#[derive(Default)]
struct VaultLink {
    host: Option<HostId>,
    closed: bool,
}

fn error(code: ReplicationErrorCode, message: &str) -> VaultMessage {
    VaultMessage::Error {
        error: ReplicationError {
            code,
            message: message.into(),
        },
    }
}

impl Vault {
    fn paired_with(host: &Host) -> Self {
        Self {
            paired: [host.host_id.clone()].into(),
            ..Self::default()
        }
    }

    fn receive(&mut self, link: &mut VaultLink, message: HostMessage) -> Vec<VaultMessage> {
        let out = self.handle(link, message);
        if out.iter().any(|m| matches!(m, VaultMessage::Error { .. })) {
            link.closed = true;
        }
        out
    }

    fn handle(&mut self, link: &mut VaultLink, message: HostMessage) -> Vec<VaultMessage> {
        if let HostMessage::Hello(hello) = &message {
            if link.host.is_some() {
                return vec![error(ReplicationErrorCode::BadRequest, "second hello")];
            }
            if !self.paired.contains(&hello.host_id) {
                return vec![error(ReplicationErrorCode::Forbidden, "not paired")];
            }
            let matches = hello.replication_version == REPLICATION_VERSION;
            let acked = self
                .journals
                .iter()
                .filter(|((host, _), _)| matches && host == &hello.host_id)
                .map(|((_, session_id), journal)| Cursor {
                    session_id: session_id.clone(),
                    after_seq: journal.len() as Seq,
                })
                .collect();
            link.host = matches.then(|| hello.host_id.clone());
            link.closed = !matches;
            return vec![VaultMessage::Hello(VaultHello {
                replication_version: REPLICATION_VERSION,
                build: "herder/0.0.0".into(),
                acked,
            })];
        }
        let Some(host) = link.host.clone() else {
            return vec![error(ReplicationErrorCode::BadRequest, "no hello")];
        };
        match message {
            HostMessage::Hello(_) => unreachable!("handled above"),
            HostMessage::Session(summary) => {
                self.summaries
                    .insert((host, summary.session_id.clone()), summary);
                Vec::new()
            }
            HostMessage::Batch(batch) => self.batch(host, batch),
            // Images are not modelled here; the daemon's vault tests cover them.
            HostMessage::Attachment(_) | HostMessage::Unknown => Vec::new(),
        }
    }

    fn batch(&mut self, host: HostId, batch: Batch) -> Vec<VaultMessage> {
        let Some(first) = batch.events.first().map(|e| e.seq) else {
            return vec![error(ReplicationErrorCode::BadRequest, "empty batch")];
        };
        let consecutive =
            (batch.events.iter().map(|e| e.seq)).eq(first..first + batch.events.len() as Seq);
        if !consecutive || first == 0 || batch.events.len() > MAX_BATCH_EVENTS {
            return vec![error(ReplicationErrorCode::BadRequest, "bad batch")];
        }
        let journal = self
            .journals
            .entry((host, batch.session_id.clone()))
            .or_default();
        let held = journal.len() as Seq;
        let cursor = Cursor {
            session_id: batch.session_id,
            after_seq: held,
        };
        if first > held + 1 {
            return vec![VaultMessage::Rejected {
                cursor,
                reason: RejectReason::Gap,
            }];
        }
        let overlap = (held + 1 - first) as usize;
        let (resent, new) = batch.events.split_at(overlap.min(batch.events.len()));
        if resent != &journal[first as usize - 1..][..resent.len()] {
            return vec![VaultMessage::Rejected {
                cursor,
                reason: RejectReason::Conflict,
            }];
        }
        // One transaction in a real vault; the ack goes out only after it commits.
        journal.extend_from_slice(new);
        self.writes += new.len();
        vec![VaultMessage::Ack(Cursor {
            after_seq: journal.len() as Seq,
            ..cursor
        })]
    }
}

/// Passes a message through JSON text, as the WebSocket does.
fn wire<T: serde::Serialize + serde::de::DeserializeOwned>(message: T) -> T {
    serde_json::from_str(&serde_json::to_string(&message).unwrap()).unwrap()
}

/// One connection between a host and the vault, with the frames in flight each way.
#[derive(Default)]
struct Connection {
    to_vault: VecDeque<HostMessage>,
    to_host: VecDeque<VaultMessage>,
    vault_link: VaultLink,
}

impl Connection {
    fn open(host: &mut Host) -> Self {
        let mut conn = Self::default();
        conn.to_vault.push_back(host.connect());
        conn
    }

    /// The vault handles the next frame from the host; false when there is none.
    fn vault_step(&mut self, vault: &mut Vault) -> bool {
        if self.vault_link.closed {
            return false;
        }
        let Some(message) = self.to_vault.pop_front() else {
            return false;
        };
        self.to_host.extend(
            vault
                .receive(&mut self.vault_link, wire(message))
                .into_iter()
                .map(wire),
        );
        true
    }

    /// The host handles the next frame from the vault; false when there is none.
    fn host_step(&mut self, host: &mut Host) -> bool {
        let Some(message) = self.to_host.pop_front() else {
            return false;
        };
        let before = host.journals.clone();
        self.to_vault
            .extend(host.receive(message).into_iter().map(wire));
        assert_eq!(
            host.journals, before,
            "a vault message changed the host's journal"
        );
        true
    }

    fn run(&mut self, host: &mut Host, vault: &mut Vault) {
        while self.vault_step(vault) | self.host_step(host) {}
    }

    /// The vault dies: frames in flight either way are lost, and so is the host's link.
    fn kill(self, host: &mut Host) {
        host.link = None;
    }
}

fn host_with_history(sessions: usize, events: usize, batch_size: usize) -> Host {
    let mut host = Host::new(batch_size);
    for s in 0..sessions {
        let session = format!("01J9SESSION{s}");
        host.append(
            &session,
            json!({ "type": "session_status_changed", "status": "running" }),
        );
        for n in 1..events {
            host.append(
                &session,
                json!({ "type": "turn_started", "turn_id": format!("T{n}") }),
            );
        }
    }
    host
}

fn assert_replicated(host: &Host, vault: &Vault) {
    for (session_id, journal) in &host.journals {
        let replica = &vault.journals[&(host.host_id.clone(), session_id.clone())];
        let seqs: Vec<Seq> = replica.iter().map(|e| e.seq).collect();
        assert_eq!(
            seqs,
            (1..=journal.len() as Seq).collect::<Vec<_>>(),
            "gap or duplicate"
        );
        assert_eq!(replica, journal);
        assert_eq!(
            vault.summaries[&(host.host_id.clone(), session_id.clone())],
            host.summary(session_id)
        );
        let link = host.link.as_ref().unwrap();
        assert_eq!(
            link.acked[session_id],
            journal.len() as Seq,
            "not fully acked"
        );
    }
    let total: usize = host.journals.values().map(Vec::len).sum();
    assert_eq!(vault.writes, total, "an event was written twice");
}

/// Done when: kill the vault mid-stream; the host resumes from the last acked cursor with no
/// gap or duplicate. Tried at every point of the stream, with the host still appending.
#[test]
fn killing_the_vault_mid_stream_loses_and_duplicates_nothing() {
    let total_steps = {
        let mut host = host_with_history(3, 20, 4);
        let mut vault = Vault::paired_with(&host);
        let mut conn = Connection::open(&mut host);
        let mut steps = 0;
        loop {
            let handled = conn.vault_step(&mut vault);
            steps += usize::from(handled);
            if !(conn.host_step(&mut host) | handled) {
                break steps;
            }
        }
    };
    assert!(total_steps > 10);
    for kill_after in 0..=total_steps {
        let mut host = host_with_history(3, 20, 4);
        let mut vault = Vault::paired_with(&host);
        let mut conn = Connection::open(&mut host);
        for step in 0..kill_after {
            conn.vault_step(&mut vault);
            // Some acks reach the host before the kill, the rest are lost with it.
            if step % 2 == 0 {
                conn.host_step(&mut host);
            }
            if step % 3 == 0 {
                host.append(
                    "01J9SESSION1",
                    json!({ "type": "turn_completed", "turn_id": "L" }),
                );
                let batches = host.pump();
                conn.to_vault.extend(batches);
            }
        }
        conn.kill(&mut host);
        host.append(
            "01J9SESSION2",
            json!({ "type": "turn_completed", "turn_id": "D" }),
        );

        let mut conn = Connection::open(&mut host);
        conn.run(&mut host, &mut vault);
        assert_replicated(&host, &vault);
    }
}

#[test]
fn new_events_stream_live_after_catching_up() {
    let mut host = host_with_history(1, 5, 2);
    let mut vault = Vault::paired_with(&host);
    let mut conn = Connection::open(&mut host);
    conn.run(&mut host, &mut vault);
    host.append(
        "01J9SESSION0",
        json!({ "type": "turn_completed", "turn_id": "T4" }),
    );
    host.append(
        "01J9SESSION9",
        json!({ "type": "session_status_changed", "status": "idle" }),
    );
    let batches = host.pump();
    assert_eq!(batches.len(), 2);
    conn.to_vault.extend(batches);
    let summary = host.summary(&SessionId::new("01J9SESSION9"));
    conn.to_vault.push_front(HostMessage::Session(summary));
    let summary = host.summary(&SessionId::new("01J9SESSION0"));
    conn.to_vault.push_front(HostMessage::Session(summary));
    conn.run(&mut host, &mut vault);
    assert_replicated(&host, &vault);
}

#[test]
fn a_resent_batch_is_acknowledged_without_writing_it_again() {
    let mut host = host_with_history(1, 6, 6);
    let mut vault = Vault::paired_with(&host);
    let mut link = VaultLink::default();
    vault.receive(&mut link, host.connect());
    let session_id = SessionId::new("01J9SESSION0");
    let batch = |from: usize, to: usize| {
        HostMessage::Batch(Batch {
            session_id: session_id.clone(),
            events: host.journals[&session_id][from..to].to_vec(),
        })
    };
    let ack = |after_seq| {
        vec![VaultMessage::Ack(Cursor {
            session_id: session_id.clone(),
            after_seq,
        })]
    };
    assert_eq!(vault.receive(&mut link, batch(0, 4)), ack(4));
    assert_eq!(vault.receive(&mut link, batch(0, 4)), ack(4));
    assert_eq!(vault.writes, 4);
    // Overlapping the held events, then new ones: only the new ones are written.
    assert_eq!(vault.receive(&mut link, batch(2, 6)), ack(6));
    assert_eq!(vault.writes, 6);
    assert_eq!(
        vault.journals[&(host.host_id.clone(), session_id.clone())],
        host.journals[&session_id]
    );
}

#[test]
fn a_conflicting_resend_is_rejected_and_changes_nothing() {
    let mut host = host_with_history(1, 4, 4);
    let mut vault = Vault::paired_with(&host);
    let mut conn = Connection::open(&mut host);
    conn.run(&mut host, &mut vault);
    let session_id = SessionId::new("01J9SESSION0");
    let held = vault.journals.clone();

    let mut events = host.journals[&session_id][2..].to_vec();
    events[1].body =
        RawEventBody::from_value(json!({ "type": "turn_started", "turn_id": "OTHER" })).unwrap();
    conn.to_vault.push_back(HostMessage::Batch(Batch {
        session_id: session_id.clone(),
        events,
    }));
    conn.vault_step(&mut vault);
    assert_eq!(
        conn.to_host.back(),
        Some(&VaultMessage::Rejected {
            cursor: Cursor {
                session_id: session_id.clone(),
                after_seq: 4,
            },
            reason: RejectReason::Conflict,
        })
    );
    assert_eq!(vault.journals, held);
    assert_eq!(vault.writes, 4);

    // The host stops replicating the conflicting session, and only that one.
    conn.run(&mut host, &mut vault);
    host.append(
        "01J9SESSION0",
        json!({ "type": "turn_completed", "turn_id": "T3" }),
    );
    host.append(
        "01J9SESSION1",
        json!({ "type": "session_status_changed", "status": "idle" }),
    );
    let batches = host.pump();
    assert!(matches!(
        &batches[..],
        [HostMessage::Batch(batch)] if batch.session_id.as_str() == "01J9SESSION1"
    ));
}

#[test]
fn a_gap_is_rejected_and_the_host_rewinds_to_the_vault_cursor() {
    let mut host = host_with_history(1, 12, 3);
    let mut vault = Vault::paired_with(&host);
    let mut conn = Connection::open(&mut host);
    conn.vault_step(&mut vault);
    conn.host_step(&mut host);
    // The second batch never arrives.
    let batches: Vec<_> = conn.to_vault.drain(..).collect();
    conn.to_vault.extend(
        batches
            .into_iter()
            .enumerate()
            .filter(|(i, _)| *i != 2)
            .map(|(_, m)| m),
    );
    conn.vault_step(&mut vault); // summary
    conn.vault_step(&mut vault); // seqs 1-3
    conn.vault_step(&mut vault); // seqs 7-9
    assert_eq!(
        conn.to_host.back(),
        Some(&VaultMessage::Rejected {
            cursor: Cursor {
                session_id: SessionId::new("01J9SESSION0"),
                after_seq: 3,
            },
            reason: RejectReason::Gap,
        })
    );
    conn.run(&mut host, &mut vault);
    assert_replicated(&host, &vault);
}

#[test]
fn a_vault_restored_from_an_older_backup_gets_the_rest_again() {
    let mut host = host_with_history(2, 10, 4);
    let mut vault = Vault::paired_with(&host);
    let mut conn = Connection::open(&mut host);
    conn.run(&mut host, &mut vault);
    conn.kill(&mut host);
    for journal in vault.journals.values_mut() {
        journal.truncate(3);
    }
    vault.writes = 6;
    let mut conn = Connection::open(&mut host);
    conn.run(&mut host, &mut vault);
    assert_replicated(&host, &vault);
}

#[test]
fn unknown_event_types_are_replicated_verbatim() {
    let mut host = host_with_history(1, 2, 4);
    let future = json!({ "type": "event_from_the_future", "detail": { "n": 1 } });
    host.append("01J9SESSION0", future.clone());
    let mut vault = Vault::paired_with(&host);
    let mut conn = Connection::open(&mut host);
    conn.run(&mut host, &mut vault);
    assert_replicated(&host, &vault);
    let replica = &vault.journals[&(host.host_id.clone(), SessionId::new("01J9SESSION0"))];
    assert_eq!(serde_json::to_value(&replica[2].body).unwrap(), future);
    assert_eq!(replica[2].body.decode(), EventBody::Unknown);
}

#[test]
fn the_vault_refuses_an_unpaired_host_and_frames_before_hello() {
    let mut host = host_with_history(1, 2, 4);
    let mut vault = Vault::default();
    let mut link = VaultLink::default();
    let refused = vault.receive(&mut link, host.connect());
    assert!(matches!(
        &refused[..],
        [VaultMessage::Error { error }] if error.code == ReplicationErrorCode::Forbidden
    ));
    assert!(link.closed);

    let mut vault = Vault::paired_with(&host);
    let mut link = VaultLink::default();
    let batch = HostMessage::Batch(Batch {
        session_id: SessionId::new("01J9SESSION0"),
        events: host.journals[&SessionId::new("01J9SESSION0")].clone(),
    });
    let refused = vault.receive(&mut link, batch);
    assert!(matches!(
        &refused[..],
        [VaultMessage::Error { error }] if error.code == ReplicationErrorCode::BadRequest
    ));
    assert!(vault.journals.is_empty());
}

#[test]
fn the_vault_refuses_malformed_batches() {
    let mut host = host_with_history(1, 4, 4);
    let session_id = SessionId::new("01J9SESSION0");
    let journal = host.journals[&session_id].clone();
    let shuffled = vec![journal[1].clone(), journal[0].clone()];
    let too_big = vec![journal[0].clone(); MAX_BATCH_EVENTS + 1]
        .into_iter()
        .enumerate()
        .map(|(i, e)| JournalRecord {
            seq: i as Seq + 1,
            ..e
        })
        .collect();
    for events in [Vec::new(), shuffled, too_big] {
        let mut vault = Vault::paired_with(&host);
        let mut link = VaultLink::default();
        vault.receive(&mut link, host.connect());
        let refused = vault.receive(
            &mut link,
            HostMessage::Batch(Batch {
                session_id: session_id.clone(),
                events,
            }),
        );
        assert!(matches!(
            &refused[..],
            [VaultMessage::Error { error }] if error.code == ReplicationErrorCode::BadRequest
        ));
        assert!(link.closed);
        assert_eq!(vault.writes, 0);
    }
}
