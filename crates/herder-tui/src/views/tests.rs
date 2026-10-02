//! Snapshots of the main screens, drawn on a test backend from fake client-core state.

use herder_client_core::ConnectionState;
use herder_protocol::{ItemBody, ItemId};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Color;
use serde_json::json;

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
    insta::assert_snapshot!(render(&mut app, 80, 20).backend());
}
