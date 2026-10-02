//! Snapshots of the main screens, drawn on a test backend from fake client-core state.

use herder_client_core::ConnectionState;
use herder_protocol::{ItemBody, ItemId};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Color;
use serde_json::json;

use crate::app::Focus;
use crate::app::{App, Msg};
use crate::fake::{self, added, assistant, item, update};

fn render(app: &mut App, width: u16, height: u16) -> Terminal<TestBackend> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| super::draw(frame, app)).unwrap();
    terminal
}

fn press(app: &mut App, code: KeyCode) {
    app.update(Msg::Key(KeyEvent::new(code, KeyModifiers::NONE)));
}

/// `s2` of [`fake::tree`] mid-turn: a prompt, some tool work, and an answer streaming.
fn mid_turn() -> App {
    let mut app = fake::tree();
    let events = vec![
        added(
            "i1",
            ItemBody::UserMessage {
                text: "Add a health endpoint and test it.".into(),
            },
        ),
        added(
            "i2",
            ItemBody::Reasoning {
                text: "The router lives in src/api.rs.".into(),
            },
        ),
        added(
            "i3",
            ItemBody::ToolCall {
                name: "Bash".into(),
                input: json!({"command": "cargo test --workspace", "timeout": 600}),
            },
        ),
        added(
            "i4",
            ItemBody::ToolResult {
                call_id: ItemId::new("i3"),
                output: "running 12 tests\ntest a ... ok\n\ntest result: ok".into(),
                is_error: false,
            },
        ),
        added(
            "i5",
            assistant(
                "## Plan\nI added `GET /health`; it returns 200 with the build version, \
                 so load balancers can probe it.\n- one route\n- one test\n```rust\nfn health() {}\n```",
            ),
        ),
    ];
    fake::feed(&mut app, "h1", "s2", update("s2", 3, events, Vec::new()));
    let streaming = vec![item("i6", assistant("Now the docs: the endpoint is"))];
    fake::feed(&mut app, "h1", "s2", update("s2", 8, Vec::new(), streaming));
    press(&mut app, KeyCode::Enter);
    app
}

#[test]
fn no_machines_shows_how_to_pair() {
    let mut app = App::default();
    insta::assert_snapshot!(render(&mut app, 80, 16).backend());
}

#[test]
fn sessions_show_status_and_the_task_tree() {
    let mut app = fake::tree();
    let terminal = render(&mut app, 80, 12);
    insta::assert_snapshot!(terminal.backend());
    // Badges carry their colour: "needs you" stands out.
    let buffer = terminal.backend().buffer();
    let needs_you = (0..80).find(|&x| buffer[(x, 2)].symbol() == "n").unwrap();
    assert_eq!(buffer[(needs_you, 2)].fg, Color::Magenta);
}

#[test]
fn a_session_streams_its_transcript() {
    let mut app = mid_turn();
    insta::assert_snapshot!(render(&mut app, 100, 30).backend());
}

#[test]
fn a_long_transcript_shows_its_end() {
    let mut app = mid_turn();
    insta::assert_snapshot!(render(&mut app, 100, 12).backend());
    // Scrolled to the top, it shows its start.
    press(&mut app, KeyCode::Char('g'));
    let terminal = render(&mut app, 100, 12);
    assert!(
        terminal
            .backend()
            .to_string()
            .contains("Add a health endpoint"),
        "{}",
        terminal.backend()
    );
}

#[test]
fn the_status_line_shows_each_connection() {
    let mut app = fake::tree();
    let mut machines = app.machines.clone();
    let mut down = fake::machine("h2", "laptop", &[]);
    down.connection = ConnectionState::Disconnected {
        error: "connection refused".into(),
    };
    let mut up = fake::machine("h3", "ci", &[]);
    up.connection = ConnectionState::Connecting;
    machines.extend([down, up]);
    app.update(Msg::Machines(machines));
    insta::assert_snapshot!(render(&mut app, 100, 10).backend());
}

#[test]
fn help_lists_the_keys() {
    let mut app = fake::tree();
    press(&mut app, KeyCode::Char('?'));
    insta::assert_snapshot!(render(&mut app, 80, 32).backend());
}

/// `s2` of [`fake::tree`] open, with `bodies` fed to it from seq 3.
fn open_s2(bodies: Vec<herder_protocol::EventBody>) -> App {
    let mut app = fake::tree();
    fake::feed(&mut app, "h1", "s2", update("s2", 3, bodies, Vec::new()));
    press(&mut app, KeyCode::Enter);
    app
}

#[test]
fn an_approval_is_prompted_and_badged() {
    let mut app = open_s2(vec![
        fake::started("turn-1"),
        added(
            "i1",
            ItemBody::UserMessage {
                text: "Clean the build.".into(),
            },
        ),
        fake::approval("a1", "Bash: rm -rf target"),
        fake::approval("a2", "Bash: cargo build"),
    ]);
    let terminal = render(&mut app, 100, 20);
    insta::assert_snapshot!(terminal.backend());
    let screen = terminal.backend().to_string();
    assert!(screen.contains("approve?"), "{screen}");
}

#[test]
fn a_question_lists_its_choices() {
    let mut app = open_s2(vec![
        fake::started("turn-1"),
        fake::question(
            "q1",
            "Which database should the cache use?",
            &["SQLite", "Postgres"],
        ),
    ]);
    insta::assert_snapshot!(render(&mut app, 100, 20).backend());
}

#[test]
fn a_prompt_sent_during_a_turn_shows_queued() {
    let mut app = open_s2(vec![fake::started("turn-1")]);
    let streaming = vec![item("i6", assistant("Working on it"))];
    fake::feed(&mut app, "h1", "s2", update("s2", 4, Vec::new(), streaming));
    press(&mut app, KeyCode::Char('i'));
    fake::type_text(&mut app, "then add docs");
    press(&mut app, KeyCode::Enter);
    fake::type_text(&mut app, "and a changelog entry");
    insta::assert_snapshot!(render(&mut app, 100, 20).backend());
}

#[test]
fn the_palette_shows_why_a_command_failed() {
    let mut app = open_s2(vec![]);
    press(&mut app, KeyCode::Char(':'));
    fake::type_text(&mut app, "mode yolo");
    press(&mut app, KeyCode::Enter);
    insta::assert_snapshot!(render(&mut app, 100, 12).backend());
}

#[test]
fn a_refused_command_shows_in_the_session_view() {
    let mut app = open_s2(vec![]);
    app.update(Msg::Sent {
        origin: crate::compose::Origin::Session(fake::key("h1", "s2")),
        result: Err("claude cannot switch models mid-session".into()),
    });
    insta::assert_snapshot!(render(&mut app, 100, 12).backend());
}

#[test]
fn the_new_session_dialog() {
    let mut app = fake::tree();
    let mut machines = app.machines.clone();
    machines[0].accounts = vec![fake::account("claude-main", "Main")];
    app.update(Msg::Machines(machines));
    press(&mut app, KeyCode::Char('n'));
    press(&mut app, KeyCode::Tab);
    press(&mut app, KeyCode::Tab);
    press(&mut app, KeyCode::Tab);
    insta::assert_snapshot!(render(&mut app, 100, 20).backend());
}

#[test]
fn the_session_list_badges_each_sessions_prs() {
    let mut app = fake::with_prs();
    let terminal = render(&mut app, 80, 10);
    insta::assert_snapshot!(terminal.backend());
    // The badge's number takes its PR state's colour, its marks the checks'.
    let buffer = terminal.backend().buffer();
    let row = |y: u16| (0..40).map(|x| buffer[(x, y)].symbol()).collect::<String>();
    let s2 = (0..12).find(|&y| row(y).contains("#7")).unwrap();
    let at = row(s2).find("#7").unwrap();
    let x = u16::try_from(row(s2)[..at].chars().count()).unwrap();
    assert_eq!(buffer[(x, s2)].fg, Color::Green);
    assert_eq!(buffer[(x + 2, s2)].symbol(), "✓");
}

#[test]
fn the_open_session_shows_its_prs_over_the_transcript() {
    let mut app = fake::with_prs();
    press(&mut app, KeyCode::Char('p'));
    press(&mut app, KeyCode::Char('j'));
    insta::assert_snapshot!(render(&mut app, 110, 14).backend());
    // Out of the strip, the strip stays and only hints at its key.
    press(&mut app, KeyCode::Esc);
    assert_eq!(app.focus, Focus::Transcript);
    let terminal = render(&mut app, 110, 14);
    let screen = terminal.backend().to_string();
    assert!(screen.contains("pull requests (2)"), "{screen}");
    assert!(!screen.contains("x unlink"), "{screen}");
}

#[test]
fn a_session_without_prs_has_no_strip() {
    let mut app = fake::with_prs();
    press(&mut app, KeyCode::Char('G'));
    press(&mut app, KeyCode::Enter);
    let screen = render(&mut app, 110, 14).backend().to_string();
    assert!(!screen.contains("pull requests"), "{screen}");
}

#[test]
fn every_sessions_prs_are_listed_under_their_session() {
    let mut app = fake::with_prs();
    press(&mut app, KeyCode::Char('P'));
    press(&mut app, KeyCode::Char('G'));
    insta::assert_snapshot!(render(&mut app, 110, 14).backend());
}

#[test]
fn no_prs_anywhere_says_so() {
    let mut app = fake::tree();
    press(&mut app, KeyCode::Char('P'));
    let screen = render(&mut app, 110, 10).backend().to_string();
    assert!(screen.contains("No pull requests are linked"), "{screen}");
}

#[test]
fn the_link_prompt_names_its_session() {
    let mut app = fake::with_prs();
    press(&mut app, KeyCode::Char('L'));
    for c in "https://github.com/acme/app/pull/31".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    insta::assert_snapshot!(render(&mut app, 90, 12).backend());
}

#[test]
fn a_notice_replaces_the_connections_until_the_next_key() {
    let mut app = fake::tree();
    app.update(Msg::Notice(
        "pull request #4 does not exist in acme/app".into(),
    ));
    let screen = render(&mut app, 90, 8).backend().to_string();
    assert!(screen.contains("#4 does not exist"), "{screen}");
    assert!(!screen.contains("connected"), "{screen}");
    press(&mut app, KeyCode::Char('j'));
    let screen = render(&mut app, 90, 8).backend().to_string();
    assert!(screen.contains("connected"), "{screen}");
}
