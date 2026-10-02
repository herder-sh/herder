//! Real shells on real pseudo-terminals, run as `/bin/sh` for a predictable prompt.

use std::time::Duration;

use herder_protocol::Role;

use super::*;

const TIMEOUT: Duration = Duration::from_secs(20);

struct Fixture {
    hub: Arc<Hub>,
    terminals: Terminals,
    /// An owner's connection, receiving terminal lists.
    lists: Arc<Outbox>,
    dir: tempfile::TempDir,
}

fn fixture() -> Fixture {
    let hub = Arc::new(Hub::default());
    let lists = Arc::new(Outbox::default());
    hub.connect(&lists, Role::Owner);
    Fixture {
        terminals: Terminals::new(Arc::clone(&hub), PathBuf::from("/bin/sh")),
        hub,
        lists,
        dir: tempfile::tempdir().unwrap(),
    }
}

impl Fixture {
    fn open(&self, outbox: &Arc<Outbox>) -> TerminalId {
        self.terminals
            .open(SessionId::new("s1"), self.dir.path(), 80, 24, outbox)
            .unwrap()
    }

    async fn type_line(&self, terminal_id: &TerminalId, outbox: &Arc<Outbox>, line: &str) {
        let data = format!("{line}\n").into_bytes();
        self.terminals
            .input(terminal_id, outbox, data)
            .await
            .unwrap();
    }

    /// Waits for a terminal to close; returns its id and exit code.
    async fn next_closed(&self) -> (TerminalId, Option<i32>) {
        match next(&self.lists).await {
            ServerMessage::TerminalClosed {
                terminal_id,
                exit_code,
            } => (terminal_id, exit_code),
            other => panic!("expected a closed terminal, got {other:?}"),
        }
    }

    /// Waits for the next terminal list.
    async fn next_list(&self) -> Vec<Terminal> {
        match next(&self.lists).await {
            ServerMessage::Terminals { terminals } => terminals,
            other => panic!("expected a terminal list, got {other:?}"),
        }
    }
}

async fn next(outbox: &Outbox) -> ServerMessage {
    tokio::time::timeout(TIMEOUT, async {
        loop {
            if let Some(message) = outbox.pop() {
                return message;
            }
            outbox.ready().await;
        }
    })
    .await
    .expect("no message")
}

/// Collects terminal output on `outbox` until it contains `needle`; returns all of it.
async fn read_until(outbox: &Outbox, needle: &str) -> String {
    let mut seen = Vec::new();
    while !String::from_utf8_lossy(&seen).contains(needle) {
        match next(outbox).await {
            ServerMessage::TerminalOutput { data, .. } => seen.extend(data.0),
            other => panic!("expected terminal output, got {other:?}"),
        }
    }
    String::from_utf8_lossy(&seen).into_owned()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reattached_client_gets_the_scrollback() {
    let f = fixture();
    let first = Arc::new(Outbox::default());
    let terminal_id = f.open(&first);
    // The command echoes as typed; only its output reads "hello".
    f.type_line(&terminal_id, &first, "printf 'he%s\\n' llo")
        .await;
    read_until(&first, "hello").await;
    f.terminals.detach(&terminal_id, &first).unwrap();
    // Whatever was queued before the detach, such as the next prompt, is not the point.
    while first.pop().is_some() {}

    let second = Arc::new(Outbox::default());
    f.terminals.attach(&terminal_id, &second).unwrap();
    let ServerMessage::TerminalOutput { data, .. } = next(&second).await else {
        panic!("expected the scrollback");
    };
    assert!(String::from_utf8_lossy(&data.0).contains("hello"));
    // The shell kept running while nobody was attached, and streams live again.
    f.type_line(&terminal_id, &second, "printf 'wor%s\\n' ld")
        .await;
    read_until(&second, "world").await;
    assert!(first.pop().is_none(), "a detached client got output");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_shell_runs_in_the_worktree_with_xterm() {
    let f = fixture();
    let outbox = Arc::new(Outbox::default());
    let terminal_id = f.open(&outbox);
    f.type_line(&terminal_id, &outbox, "echo \"[$TERM:$(pwd)]\"")
        .await;
    let dir = f.dir.path().canonicalize().unwrap();
    read_until(&outbox, &format!("[xterm-256color:{}]", dir.display())).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resize_changes_the_size_the_shell_sees() {
    let f = fixture();
    let outbox = Arc::new(Outbox::default());
    let terminal_id = f.open(&outbox);
    f.terminals.resize(&terminal_id, &outbox, 123, 45).unwrap();
    f.type_line(&terminal_id, &outbox, "stty size").await;
    read_until(&outbox, "45 123").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_attached_clients_type_or_resize() {
    let f = fixture();
    let attached = Arc::new(Outbox::default());
    let terminal_id = f.open(&attached);
    let stranger = Arc::new(Outbox::default());
    let err = f
        .terminals
        .input(&terminal_id, &stranger, b"x".to_vec())
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict);
    let err = f
        .terminals
        .resize(&terminal_id, &stranger, 1, 1)
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict);
    let err = f
        .terminals
        .resize(&terminal_id, &attached, 0, 1)
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::BadRequest);
    let missing = TerminalId::new("missing");
    let err = f.terminals.attach(&missing, &stranger).unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_shell_exit_closes_the_terminal_and_updates_the_list() {
    let f = fixture();
    let outbox = Arc::new(Outbox::default());
    let terminal_id = f.open(&outbox);
    let opened = f.next_list().await;
    assert_eq!(
        opened,
        [Terminal {
            terminal_id: terminal_id.clone(),
            purpose: TerminalPurpose::Shell {
                session_id: SessionId::new("s1"),
            },
        }]
    );
    assert_eq!(f.terminals.list(), opened);
    f.type_line(&terminal_id, &outbox, "exit 3").await;
    assert_eq!(f.next_closed().await, (terminal_id.clone(), Some(3)));
    assert_eq!(f.next_list().await, []);
    assert!(f.terminals.list().is_empty());
    let err = f.terminals.attach(&terminal_id, &outbox).unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn archiving_a_session_closes_its_terminals() {
    let f = fixture();
    let outbox = Arc::new(Outbox::default());
    let terminal_id = f.open(&outbox);
    f.next_list().await;
    let sink = KillOnArchive {
        next: Arc::clone(&f.hub) as Arc<dyn EventSink>,
        terminals: f.terminals.clone(),
    };
    let status = |status| Event {
        session_id: SessionId::new("s1"),
        seq: 2,
        at: jiff::Timestamp::UNIX_EPOCH,
        by: None,
        body: EventBody::SessionStatusChanged { status },
    };
    sink.event(&status(SessionStatus::Idle));
    assert_eq!(f.terminals.list().len(), 1);
    sink.event(&status(SessionStatus::Archived));
    // The hang-up kills the shell: a signal, not an exit code.
    assert_eq!(f.next_closed().await, (terminal_id, None));
    assert_eq!(f.next_list().await, []);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_client_that_falls_behind_is_let_go() {
    let f = fixture();
    let slow = Arc::new(Outbox::default());
    let terminal_id = f.open(&slow);
    let fast = Arc::new(Outbox::default());
    f.terminals.attach(&terminal_id, &fast).unwrap();
    let term = f.terminals.get(&terminal_id).unwrap();
    // `slow` never reads; `fast` keeps up.
    for _ in 0..=crate::hub::TERMINAL_BACKLOG {
        term.publish(&terminal_id, b"x");
        while fast.pop().is_some() {}
    }
    assert_eq!(slow.state(), crate::hub::OutboxState::Overflowed);
    let output = lock(&term.output);
    assert!(!output.is_attached(&slow));
    assert!(output.is_attached(&fast));
}

#[test]
fn scrollback_keeps_only_the_latest_bytes() {
    let mut scrollback = VecDeque::new();
    keep(&mut scrollback, &vec![b'a'; SCROLLBACK]);
    keep(&mut scrollback, b"tail");
    assert_eq!(scrollback.len(), SCROLLBACK);
    assert!(scrollback.iter().rev().take(5).eq(b"liata".iter()));
}
