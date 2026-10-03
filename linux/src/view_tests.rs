//! The session view in the window, fed the screenshots' demo session, with a sender that
//! records the commands instead of sending them.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;

use herder_protocol::{
    AccountId, Answer, ApprovalDecision, ApprovalId, CommandBody, EventBody, HostId,
    PermissionMode, QuestionId, SessionId, SessionStatus, Timestamp,
};

use crate::lists::SessionKey;
use crate::screenshots::{machines, moment, settle};
use crate::window::MainWindow;

fn api() -> SessionKey {
    SessionKey {
        host_id: HostId::new("h1"),
        session_id: SessionId::new("s-api"),
    }
}

/// A window showing the api session at `moment`, and the commands it sends.
fn open(name: &str) -> (MainWindow, Rc<RefCell<Vec<CommandBody>>>) {
    adw::init().expect("libadwaita initializes");
    let window = MainWindow::new(None);
    let sent = Rc::new(RefCell::new(Vec::new()));
    let log = Rc::clone(&sent);
    window.set_sender(Rc::new(move |host_id, command| {
        assert_eq!(host_id, HostId::new("h1"));
        log.borrow_mut().push(command);
        Box::pin(async { Ok(()) })
    }));
    let (status, update) = moment(name);
    window.show_machines(&machines(status));
    window.apply(&api(), &update);
    window.open(&api());
    (window, sent)
}

fn has(texts: &[String], wanted: &str) -> bool {
    texts.iter().any(|text| text.contains(wanted))
}

#[gtk::test]
fn the_transcript_draws_items_tools_and_the_reply_footer() {
    let (window, _) = open("chat");
    let view = window.session_view();
    assert_eq!(view.key(), Some(api()));
    let texts = view.transcript_texts();
    for wanted in [
        "Add a health endpoint and test it.",
        "+ Thought: Where the router lives: src/api.rs builds it with Router::new.",
        "src/api.rs",
        "\"Router::new\" in src (3 matches)",
        "+6",
        "Run the tests",
        "cargo test --workspace",
        "20s",
        "Todos",
        "write the tests",
        "claude-main · claude-opus-4 · 1m 32s",
        "#12",
        // The reply streaming now, with its cursor.
        "Adding a Health checks section to README.md, after ▌",
    ] {
        assert!(has(&texts, wanted), "{wanted} in {texts:?}");
    }
    // Tool calls are one row each until expanded.
    assert!(!has(&texts, "running 12 tests"), "{texts:?}");
    assert!(!has(&texts, "… 5 more lines"), "{texts:?}");
    view.toggle_tool("Run the tests");
    let texts = view.transcript_texts();
    assert!(has(&texts, "running 12 tests"), "{texts:?}");
    assert!(has(&texts, "… 5 more lines"), "{texts:?}");
    assert!(!has(&texts, "test result: ok. 12 passed; 0 failed"));
    view.toggle_tool("Run the tests");
    assert!(!has(&view.transcript_texts(), "running 12 tests"));
    // Inline Markdown is drawn, not shown.
    assert!(
        has(&texts, "I added GET /health; it returns 200"),
        "{texts:?}"
    );
    assert_eq!(view.bottom_child().as_deref(), Some("composer"));
}

#[gtk::test]
fn a_tool_call_is_redrawn_when_its_result_comes() {
    let (window, _) = open("chat");
    let view = window.session_view();
    let call = crate::session::tests::item(
        "late",
        herder_protocol::ItemBody::ToolCall {
            name: "Read".to_owned(),
            input: serde_json::json!({"file_path": "/home/dev/.herder/worktrees/api/README.md"}),
        },
    );
    let mut update = crate::session::tests::events(vec![(None, call)]);
    update.events[0].seq = 100;
    window.apply(&api(), &update);
    let done = |texts: Vec<String>| texts.iter().filter(|text| *text == "✓").count();
    let before = done(view.transcript_texts());
    let result = crate::session::tests::item(
        "late-r",
        herder_protocol::ItemBody::ToolResult {
            call_id: herder_protocol::ItemId::new("late"),
            output: "# app".to_owned(),
            is_error: false,
        },
    );
    let mut update = crate::session::tests::events(vec![(None, result)]);
    update.events[0].seq = 101;
    window.apply(&api(), &update);
    let texts = view.transcript_texts();
    assert_eq!(done(texts.clone()), before + 1, "{texts:?}");
    assert!(has(&texts, "README.md"));
}

#[gtk::test]
fn an_approval_replaces_the_composer_and_its_buttons_answer_it() {
    let (window, sent) = open("approval");
    let view = window.session_view();
    assert_eq!(view.bottom_child().as_deref(), Some("request"));
    let texts = view.request_texts();
    for wanted in [
        "△ approval",
        "Bash",
        // Asked 12 s before the fixture was built; a second may have passed since.
        "asked 1",
        "$ rm -rf target/",
        "Allow",
        "Deny",
    ] {
        assert!(has(&texts, wanted), "{wanted} in {texts:?}");
    }
    view.click("Allow");
    settle(Duration::from_millis(50));
    assert_eq!(
        *sent.borrow(),
        [CommandBody::AnswerApproval {
            session_id: SessionId::new("s-api"),
            approval_id: ApprovalId::new("ap1"),
            decision: ApprovalDecision::Allow,
        }]
    );

    // Once resolved, the composer is back.
    let mut update = crate::session::tests::events(vec![(
        Some("dev"),
        EventBody::ApprovalResolved {
            approval_id: ApprovalId::new("ap1"),
            decision: ApprovalDecision::Allow.into(),
            answered_by: herder_protocol::Answerer::User,
        },
    )]);
    update.events[0].seq = 100;
    update.events[0].at = Timestamp::now();
    window.apply(&api(), &update);
    assert_eq!(view.bottom_child().as_deref(), Some("composer"));
    assert!(has(&view.transcript_texts(), "△ allowed Bash · by you"));
}

#[gtk::test]
fn a_question_is_answered_by_a_choice_or_in_words() {
    let (window, sent) = open("question");
    let view = window.session_view();
    let texts = view.request_texts();
    for wanted in [
        "? question",
        "Which heading level should the API page use?",
        "h2 under Reference",
        "h1, its own page",
    ] {
        assert!(has(&texts, wanted), "{wanted} in {texts:?}");
    }
    view.answer_choice(1);
    view.answer_text("an h3");
    settle(Duration::from_millis(50));
    let question = |answer| CommandBody::AnswerQuestion {
        session_id: SessionId::new("s-api"),
        question_id: QuestionId::new("q1"),
        answer,
    };
    assert_eq!(
        *sent.borrow(),
        [
            question(Answer::Choice { index: 1 }),
            question(Answer::Text {
                text: "an h3".to_owned()
            }),
        ]
    );
}

#[gtk::test]
fn the_composer_sends_prompts_which_wait_marked_until_they_join() {
    let (window, sent) = open("chat");
    let view = window.session_view();
    view.submit("  Also add a changelog entry.\n");
    view.submit("   ");
    settle(Duration::from_millis(50));
    assert_eq!(
        *sent.borrow(),
        [CommandBody::SendPrompt {
            session_id: SessionId::new("s-api"),
            text: "Also add a changelog entry.".to_owned(),
            images: Vec::new(),
        }]
    );
    // A turn runs: the prompt waits, queued.
    let texts = view.transcript_texts();
    assert!(has(&texts, "QUEUED"), "{texts:?}");
    let message = crate::session::tests::item(
        "u3",
        herder_protocol::ItemBody::UserMessage {
            text: "Also add a changelog entry.".to_owned(),
            attachments: Vec::new(),
        },
    );
    let mut update = crate::session::tests::events(vec![(Some("dev"), message)]);
    update.events[0].seq = 100;
    window.apply(&api(), &update);
    let texts = view.transcript_texts();
    assert!(!has(&texts, "QUEUED"), "{texts:?}");
    assert_eq!(
        texts
            .iter()
            .filter(|t| *t == "Also add a changelog entry.")
            .count(),
        1
    );

    // A refusal takes the prompt back off.
    window.set_sender(Rc::new(|_, _| {
        Box::pin(async { Err("the session is archived".to_owned()) })
    }));
    view.submit("One more.");
    assert!(has(&view.transcript_texts(), "One more."));
    settle(Duration::from_millis(50));
    assert!(!has(&view.transcript_texts(), "One more."));
}

#[gtk::test]
fn the_controls_switch_account_provider_model_and_mode() {
    let (window, sent) = open("chat");
    let view = window.session_view();
    assert_eq!(
        view.controls(),
        ("claude-main".into(), "claude-opus-4".into(), "ask".into())
    );
    // Same provider: the conversation continues; another provider replays the transcript.
    view.pick_account(1);
    view.pick_account(2);
    // The current account changes nothing.
    view.pick_account(0);
    view.pick_model("claude-sonnet-4");
    view.pick_model("claude-opus-4");
    view.pick_mode(PermissionMode::AutoEdit);
    view.pick_mode(PermissionMode::Ask);
    settle(Duration::from_millis(50));
    let s = || SessionId::new("s-api");
    assert_eq!(
        *sent.borrow(),
        [
            CommandBody::SwitchAccount {
                session_id: s(),
                account_id: AccountId::new("claude-alt"),
            },
            CommandBody::SwitchProvider {
                session_id: s(),
                account_id: AccountId::new("codex-work"),
                model: None,
            },
            CommandBody::SetModel {
                session_id: s(),
                model: "claude-sonnet-4".to_owned(),
            },
            CommandBody::SetPermissionMode {
                session_id: s(),
                mode: PermissionMode::AutoEdit,
            },
        ]
    );
}

#[gtk::test]
fn an_archived_session_or_a_vaults_has_no_composer() {
    let (window, _) = open("chat");
    let view = window.session_view();
    let (_, update) = moment("chat");
    let mut fleet = machines(SessionStatus::Archived);
    window.show_machines(&fleet);
    window.apply(&api(), &update);
    assert_eq!(view.bottom_child().as_deref(), Some("read-only"));
    fleet = machines(SessionStatus::Idle);
    window.show_machines(&fleet);
    assert_eq!(view.bottom_child().as_deref(), Some("composer"));
    // Gone from its machine, it closes.
    fleet[0]
        .sessions
        .retain(|head| head.session_id.as_str() != "s-api");
    window.show_machines(&fleet);
    assert_eq!(view.key(), None);
}

#[gtk::test]
fn the_sessions_prs_list_over_the_transcript_open_link_and_unlink() {
    let (window, sent) = open("chat");
    let view = window.session_view();
    let texts = view.pr_texts();
    for wanted in [
        "#9",
        "Document the health endpoint",
        "draft",
        "… ci",
        "#12",
        "Add a health endpoint",
        "open",
        "✓ ci",
        "… review",
        "✓ merge",
        "herder/api",
    ] {
        assert!(
            texts.iter().any(|text| text == wanted),
            "{wanted} in {texts:?}"
        );
    }
    // Live first, in the order they were linked.
    let rows = view.pr_rows();
    assert_eq!(rows.len(), 2);

    // A row opens its PR in the browser.
    rows[0].emit_by_name::<()>("activate", &[]);
    crate::prs::OPENED.with(|opened| {
        assert_eq!(
            opened.borrow().last().map(String::as_str),
            Some("https://github.com/org/app/pull/12")
        );
    });

    // Its menu unlinks it, once confirmed.
    rows[0]
        .activate_action("pr.unlink", None)
        .expect("the row unlinks");
    let dialog = window
        .root()
        .visible_dialog()
        .and_downcast::<adw::AlertDialog>()
        .expect("the confirmation");
    assert_eq!(dialog.heading().as_deref(), Some("Unlink #12?"));
    dialog.emit_by_name::<()>("response", &[&"unlink"]);
    dialog.force_close();

    // The session's menu links another by its link.
    view.link_pr_for_test();
    let dialog = window
        .root()
        .visible_dialog()
        .and_downcast::<adw::AlertDialog>()
        .expect("the link dialog");
    assert!(!dialog.is_response_enabled("link"));
    let entry = dialog
        .extra_child()
        .and_downcast::<gtk::Entry>()
        .expect("its entry");
    entry.set_text("https://github.com/org/app/pull/41");
    assert!(dialog.is_response_enabled("link"));
    dialog.emit_by_name::<()>("response", &[&"link"]);
    dialog.force_close();
    settle(Duration::from_millis(50));
    assert_eq!(
        *sent.borrow(),
        [
            CommandBody::UnlinkPr {
                session_id: SessionId::new("s-api"),
                number: 12,
            },
            CommandBody::LinkPr {
                session_id: SessionId::new("s-api"),
                number: 41,
            },
        ]
    );

    // Read-only, it can neither link nor unlink.
    window.show_machines(&machines(SessionStatus::Archived));
    assert!(!view.can_link_pr());
    let rows = view.pr_rows();
    assert!(rows[0].activate_action("pr.unlink", None).is_err());
}

#[gtk::test]
fn the_list_goes_once_every_pr_is_unlinked() {
    let (window, _) = open("chat");
    let view = window.session_view();
    let mut update = crate::session::tests::events(vec![
        (None, EventBody::PrUnlinked { number: 12 }),
        (None, EventBody::PrUnlinked { number: 9 }),
    ]);
    for (event, seq) in update.events.iter_mut().zip(100..) {
        event.seq = seq;
    }
    window.apply(&api(), &update);
    assert!(view.pr_texts().is_empty());
    assert!(has(&view.transcript_texts(), "pull request #12 unlinked"));
}
