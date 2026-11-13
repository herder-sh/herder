//! `fake_daemon --load NAME SESSIONS EVENTS`: a machine with the journal of one that has been in
//! use for months, for measuring how the apps open and resume against a real fleet's size.
//!
//! The journal is written straight into the daemon's store before it starts: `SESSIONS`
//! sessions, the first with `EVENTS` events, one in twenty with [`LONG`], the rest with
//! [`SHORT`]; five in six are archived, as most of a used machine's are. An event is about a
//! kilobyte, as a real transcript's: prose with a list and a code block, a tool call, its output.

use std::cell::Cell;
use std::time::Duration;

use anyhow::Result;
use herder_protocol::{
    AccountId, EventBody, Item, ItemBody, ItemId, PermissionMode, Provider, SessionId,
    SessionStatus, Timestamp, TitleSource, TurnId, UserId,
};
use herder_store::{NewEvent, Store};
use serde_json::json;

use crate::support;

/// Events of one session in twenty.
const LONG: usize = 1_500;

/// Events of most sessions.
const SHORT: usize = 120;

/// Rounds of message, tool call and tool result in a turn.
const ROUNDS: usize = 12;

/// Writes `sessions` sessions into `store`, working in `repo`; the first has `events` events.
/// The newest session is the first: its last event is a minute old, the oldest's months.
pub fn seed(
    store: &mut Store,
    host: &str,
    repo: &str,
    sessions: usize,
    events: usize,
) -> Result<()> {
    let now = Timestamp::now();
    for index in 0..sessions {
        let length = match index {
            0 => events,
            _ if index % 20 == 1 => LONG,
            _ => SHORT,
        };
        // Sessions an hour apart, the newest first.
        let end = now - Duration::from_secs(60 + 3600 * index as u64);
        let archived = index % 6 != 0;
        session(store, host, repo, index, length, end, archived)?;
    }
    Ok(())
}

fn session(
    store: &mut Store,
    host: &str,
    repo: &str,
    index: usize,
    length: usize,
    end: Timestamp,
    archived: bool,
) -> Result<()> {
    let session_id = SessionId::new(format!("{host}-load-{index:04}"));
    let by = Some(UserId::new(support::OWNER));
    let count = Cell::new(0usize);
    let append = |store: &mut Store, by: Option<UserId>, body: EventBody| -> Result<()> {
        // Two seconds an event, ending at `end`.
        let at = end - Duration::from_secs(2 * length.saturating_sub(count.get()) as u64);
        count.set(count.get() + 1);
        store.append(NewEvent {
            session_id: session_id.clone(),
            at,
            by,
            body,
        })?;
        Ok(())
    };
    append(
        store,
        by.clone(),
        EventBody::SessionCreated {
            repo: repo.into(),
            worktree: repo.into(),
            branch: None,
            provider: Provider::Other(support::ACCOUNT.into()),
            account_id: AccountId::new(support::ACCOUNT),
            model: "fake-1".into(),
            permission_mode: PermissionMode::AutoEdit,
            parent: None,
            parent_host: None,
            task: None,
            failover_pin: None,
            chat: false,
            origin: None,
        },
    )?;
    append(
        store,
        None,
        EventBody::TitleChanged {
            title: format!("Load session {index} on {host}"),
            source: TitleSource::Auto,
        },
    )?;
    let mut turn = 0;
    while count.get() < length {
        turn += 1;
        let turn_id = TurnId::new(format!("{session_id}-turn-{turn}"));
        let item = |id: String, body: ItemBody| EventBody::ItemAdded {
            item: Item {
                agent_message: None,
                follow_up: None,
                parent_call_id: None,
                id: ItemId::new(id),
                turn_id: turn_id.clone(),
                body,
            },
        };
        let status = |status| EventBody::SessionStatusChanged {
            status,
            retry_at: None,
        };
        append(store, None, status(SessionStatus::Running))?;
        append(
            store,
            None,
            EventBody::TurnStarted {
                turn_id: turn_id.clone(),
            },
        )?;
        append(
            store,
            by.clone(),
            item(
                format!("{turn_id}-prompt"),
                ItemBody::UserMessage {
                    text: format!(
                        "Step {turn}: make the retry cap hold for every handler, then run the tests."
                    ),
                    attachments: Vec::new(),
                },
            ),
        )?;
        for round in 1..=ROUNDS {
            let call = format!("{turn_id}-{round}-call");
            append(
                store,
                None,
                item(
                    format!("{turn_id}-{round}-message"),
                    ItemBody::AssistantMessage {
                        text: format!(
                            "Round {round}: the retry worker reads `max_attempts` from the queue \
                             config, so the cap holds for **every** handler.\n\n\
                             - `src/jobs/retry.rs` backs off exponentially, with jitter\n\
                             - `src/webhooks/stripe.rs` acknowledges before it persists\n\n\
                             ```rust\nlet delay = base * 2u32.pow(attempt.min(8)) + jitter();\n```"
                        ),
                    },
                ),
            )?;
            append(
                store,
                None,
                item(
                    call.clone(),
                    ItemBody::ToolCall {
                        name: "Bash".into(),
                        input: json!({ "command": "cargo test -p webhooks retry" }),
                    },
                ),
            )?;
            append(
                store,
                None,
                item(
                    format!("{turn_id}-{round}-result"),
                    ItemBody::ToolResult {
                        call_id: ItemId::new(call),
                        output: OUTPUT.repeat(24),
                        is_error: false,
                    },
                ),
            )?;
        }
        append(
            store,
            None,
            EventBody::TurnCompleted {
                turn_id: turn_id.clone(),
                usage: None,
            },
        )?;
        append(store, None, status(SessionStatus::Idle))?;
    }
    if archived {
        append(
            store,
            by,
            EventBody::SessionStatusChanged {
                status: SessionStatus::Archived,
                retry_at: None,
            },
        )?;
    }
    Ok(())
}

/// A line of a test run's output; a tool result holds a couple of kilobytes of them.
const OUTPUT: &str = "test webhooks::retry::caps_attempts_for_every_handler ... ok (0.04s) [worker 3, shard 2 of 8]\n";
