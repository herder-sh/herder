//! Drags, double- and triple-clicks on a drawn transcript, and the exact text they copy.

use herder_protocol::ItemBody;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::style::Modifier;

use crate::app::{App, Effect, Focus, Msg};
use crate::fake::{self, added, assistant, update};

/// A desktop terminal: the sidebar beside the transcript.
const SIZE: (u16, u16) = (120, 40);

/// The paragraph is long enough to wrap in the transcript pane.
const REPLY: &str = "The router gains a health route that load balancers probe every few \
                     seconds, so a dead instance leaves the pool quickly, and a test in \
                     tests/api.rs covers it.\n\n```rust\nfn health() -> &'static str {\n    \
                     \"ok\"\n}\n```\n\nDone.";

/// `s2` of [`fake::tree`] open, its last reply [`REPLY`].
fn open() -> App {
    let mut app = fake::tree();
    let bodies = vec![added(
        "m1",
        ItemBody::UserMessage {
            text: "Add a health route.".into(),
            attachments: Vec::new(),
        },
    )];
    fake::feed(&mut app, "h1", "s2", update("s2", 3, bodies, Vec::new()));
    let bodies = vec![added("m2", assistant(REPLY))];
    fake::feed(&mut app, "h1", "s2", update("s2", 4, bodies, Vec::new()));
    app.update(Msg::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
    if app.focus == Focus::Composer {
        app.update(Msg::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
    }
    app
}

fn draw(app: &mut App) -> Terminal<TestBackend> {
    let mut terminal = Terminal::new(TestBackend::new(SIZE.0, SIZE.1)).unwrap();
    terminal
        .draw(|frame| crate::views::draw(frame, app))
        .unwrap();
    terminal
}

/// Where `text` starts on the screen, the first time.
fn spot(app: &mut App, text: &str) -> (u16, u16) {
    let terminal = draw(app);
    let buffer = terminal.backend().buffer();
    let wanted: Vec<String> = text.chars().map(String::from).collect();
    for y in 0..SIZE.1 {
        let row: Vec<&str> = (0..SIZE.0).map(|x| buffer[(x, y)].symbol()).collect();
        for x in 0..row.len().saturating_sub(wanted.len() - 1) {
            if row[x..x + wanted.len()]
                .iter()
                .zip(&wanted)
                .all(|(a, b)| a == b)
            {
                return (u16::try_from(x).unwrap(), y);
            }
        }
    }
    panic!("no {text:?}:\n{}", terminal.backend());
}

fn mouse(app: &mut App, kind: MouseEventKind, (column, row): (u16, u16)) -> Vec<Effect> {
    app.update(Msg::Mouse(MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }))
}

fn click(app: &mut App, at: (u16, u16)) -> Vec<Effect> {
    mouse(app, MouseEventKind::Down(MouseButton::Left), at);
    mouse(app, MouseEventKind::Up(MouseButton::Left), at)
}

#[test]
fn a_drag_copies_the_text_with_wrapped_lines_joined_and_code_kept() {
    let mut app = open();
    let from = spot(&mut app, "The router");
    let code = spot(&mut app, "fn health()");
    // The paragraph wraps: its first row stops short of its last words.
    assert!(spot(&mut app, "tests/api.rs").1 > from.1);
    // Down to the code's closing brace, two rows under `fn`.
    let to = (code.0, code.1 + 2);
    assert_eq!(
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), from),
        []
    );
    mouse(
        &mut app,
        MouseEventKind::Drag(MouseButton::Left),
        (60, from.1 + 1),
    );
    mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), to);
    let copied = mouse(&mut app, MouseEventKind::Up(MouseButton::Left), to);
    let paragraph = REPLY.split("\n\n").next().unwrap();
    assert_eq!(
        copied,
        [Effect::Copy(format!(
            "{paragraph}\n\nfn health() -> &'static str {{\n    \"ok\"\n}}"
        ))]
    );
    assert_eq!(app.notice.as_deref(), Some("copied"));
    // The selection stays highlighted, in the transcript only.
    let terminal = draw(&mut app);
    let buffer = terminal.backend().buffer();
    assert!(buffer[from].modifier.contains(Modifier::REVERSED));
    assert!(buffer[to].modifier.contains(Modifier::REVERSED));
    assert!(!buffer[(0, from.1)].modifier.contains(Modifier::REVERSED));
    assert!(
        !buffer[(to.0 + 1, to.1)]
            .modifier
            .contains(Modifier::REVERSED)
    );
    // A key clears it.
    app.update(Msg::Key(KeyEvent::new(
        KeyCode::Char('k'),
        KeyModifiers::NONE,
    )));
    assert_eq!(app.selection, None);
}

#[test]
fn a_drag_starting_on_a_prompt_leaves_its_bar_out() {
    let mut app = open();
    let from = spot(&mut app, "Add a health");
    let to = spot(&mut app, "router gains");
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        (from.0 - 2, from.1),
    );
    mouse(
        &mut app,
        MouseEventKind::Drag(MouseButton::Left),
        (to.0 + 5, to.1),
    );
    let copied = mouse(
        &mut app,
        MouseEventKind::Up(MouseButton::Left),
        (to.0 + 5, to.1),
    );
    // The prompt's padding rows and the blank between are empty lines.
    let [Effect::Copy(text)] = &copied[..] else {
        panic!("{copied:?}");
    };
    assert!(text.starts_with("Add a health route.\n"), "{text:?}");
    assert!(text.ends_with("\nThe router"), "{text:?}");
    assert!(!text.contains('┃') && !text.contains('│'), "{text:?}");
}

#[test]
fn a_drag_past_the_bottom_edge_scrolls() {
    let mut app = open();
    // Away from the end, so there is room to scroll down.
    app.scroll.top = Some(0);
    let from = spot(&mut app, "Add a health");
    let first = app.scroll.first_line();
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), from);
    let area = app.hits.text.unwrap();
    for _ in 0..3 {
        mouse(
            &mut app,
            MouseEventKind::Drag(MouseButton::Left),
            (from.0, area.bottom() + 1),
        );
    }
    let last_page = app.scroll.total.saturating_sub(app.scroll.height);
    assert_eq!(app.scroll.first_line(), (first + 3).min(last_page));
}

#[test]
fn a_double_click_copies_a_word_and_a_triple_click_its_line() {
    let mut app = open();
    let at = spot(&mut app, "tests/api.rs");
    let at = (at.0 + 7, at.1);
    assert_eq!(click(&mut app, at), []);
    assert_eq!(
        click(&mut app, at),
        [Effect::Copy("tests/api.rs".to_owned())]
    );
    // The third click takes the whole line, joined back from the rows it wrapped onto.
    let paragraph = REPLY.split("\n\n").next().unwrap();
    assert_eq!(click(&mut app, at), [Effect::Copy(paragraph.to_owned())]);
}

#[test]
fn a_click_without_a_drag_still_taps() {
    let mut app = open();
    let at = spot(&mut app, "The router");
    assert_eq!(click(&mut app, at), []);
    assert_eq!(app.selection, None);
    // The tap moved the item cursor to the reply, as before.
    assert_eq!(app.chat.cursor, Some(herder_protocol::ItemId::new("m2")));
}

#[test]
fn on_a_phone_a_drag_still_scrolls() {
    let mut app = open();
    let mut terminal = Terminal::new(TestBackend::new(45, 40)).unwrap();
    terminal
        .draw(|frame| crate::views::draw(frame, &mut app))
        .unwrap();
    let area = app.hits.text.unwrap();
    let at = (area.x + 4, area.y + 10);
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), at);
    mouse(
        &mut app,
        MouseEventKind::Drag(MouseButton::Left),
        (at.0, at.1 - 1),
    );
    assert_eq!(app.selection, None);
    // The press is the finger's: dragged, it scrolls as the wheel does.
    assert_eq!(app.dragged, Some((at.0, at.1, at.1 - 1)));
}
