//! Driving the TUI with only the keys a phone SSH app's gestures send, as Termius does: no
//! letters, no mouse.

use herder_protocol::{ApprovalDecision, CommandBody};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::{App, Effect, Focus, Msg};
use crate::fake::{self, started, update};

/// A phone in portrait, as small as herder lays out for.
const PHONE: (u16, u16) = (45, 24);

/// Draws the screen as the event loop does after every input; returns its button bar, the
/// last row.
fn draw(app: &mut App, (width, height): (u16, u16)) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| crate::views::draw(frame, app))
        .unwrap();
    let buffer = terminal.backend().buffer();
    (0..width)
        .map(|x| buffer[(x, height - 1)].symbol())
        .collect()
}

/// Presses `code` on a phone-sized screen, then draws.
fn press(app: &mut App, code: KeyCode) -> Vec<Effect> {
    let effects = app.update(Msg::Key(KeyEvent::new(code, KeyModifiers::NONE)));
    draw(app, PHONE);
    effects
}

/// [`fake::with_prs`] with `s2` asking to run a command.
fn asking() -> App {
    let mut app = fake::with_prs();
    let events = vec![started("turn-1"), fake::approval("a1", "Bash: ls")];
    fake::feed(&mut app, "h1", "s2", update("s2", 5, events, Vec::new()));
    draw(&mut app, PHONE);
    app
}

#[test]
fn list_session_approve_back_inbox_and_prs_with_gesture_keys_only() {
    let mut app = asking();
    assert_eq!(app.focus, Focus::Sessions);

    // The list: Enter opens the first session, which asks.
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Up);
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.open, Some(fake::key("h1", "s2")));
    assert_eq!(app.focus, Focus::Transcript);

    // The transcript scrolls with arrows and pages.
    press(&mut app, KeyCode::PageUp);
    press(&mut app, KeyCode::End);
    assert_eq!(app.scroll.top, None);

    // Approving is Tab, Enter.
    press(&mut app, KeyCode::Tab);
    assert_eq!(app.bar_focus, Some(0));
    let effects = press(&mut app, KeyCode::Enter);
    assert!(
        matches!(
            effects.as_slice(),
            [Effect::Send {
                command: CommandBody::AnswerApproval {
                    decision: ApprovalDecision::Allow,
                    ..
                },
                ..
            }]
        ),
        "{effects:?}"
    );
    assert_eq!(app.bar_focus, None);

    // Back to the list.
    press(&mut app, KeyCode::Esc);
    assert_eq!(app.focus, Focus::Sessions);

    // Shift-Tab wraps to the bar's end, where the inbox and the PRs are, off screen until the
    // focus reaches them.
    let bar = draw(&mut app, PHONE);
    assert!(!bar.contains("inbox"), "{bar}");
    for _ in 0..4 {
        press(&mut app, KeyCode::BackTab);
    }
    let bar = draw(&mut app, PHONE);
    assert!(bar.contains("inbox"), "{bar}");
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.focus, Focus::Inbox);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Home);

    // Out of the inbox to the open session, then the list, then every PR.
    press(&mut app, KeyCode::Esc);
    assert_eq!(app.focus, Focus::Transcript);
    press(&mut app, KeyCode::Backspace);
    assert_eq!(app.focus, Focus::Sessions);
    for _ in 0..3 {
        press(&mut app, KeyCode::BackTab);
    }
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.focus, Focus::AllPrs);
    press(&mut app, KeyCode::PageDown);
    press(&mut app, KeyCode::Home);
    assert_eq!(
        press(&mut app, KeyCode::Enter),
        [Effect::OpenUrl("https://github.com/acme/app/pull/7".into())]
    );
    press(&mut app, KeyCode::Esc);
    assert_ne!(app.focus, Focus::AllPrs);
}

#[test]
fn tab_wraps_round_the_bar_and_any_other_key_drops_its_focus() {
    let mut app = asking();
    press(&mut app, KeyCode::BackTab);
    let last = app.bar.len() - 1;
    assert_eq!(app.bar_focus, Some(last));
    press(&mut app, KeyCode::Tab);
    assert_eq!(app.bar_focus, Some(0));
    press(&mut app, KeyCode::BackTab);
    assert_eq!(app.bar_focus, Some(last));

    // Down moves the list, as ever, and Enter then opens, into the prompt.
    press(&mut app, KeyCode::Down);
    assert_eq!(app.bar_focus, None);
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.focus, Focus::Composer);
}

#[test]
fn the_focus_drops_when_the_bar_changes() {
    let mut app = asking();
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Tab);
    assert_eq!(app.bar_focus, Some(0));
    // The approval is answered: its buttons go.
    let answered = vec![herder_protocol::EventBody::ApprovalResolved {
        approval_id: herder_protocol::ApprovalId::new("a1"),
        decision: herder_protocol::ApprovalOutcome::Allow,
        answered_by: Default::default(),
    }];
    fake::feed(&mut app, "h1", "s2", update("s2", 6, answered, Vec::new()));
    draw(&mut app, PHONE);
    assert_eq!(app.bar_focus, None);
}

#[test]
fn on_a_wide_screen_tab_still_switches_panes() {
    let mut app = asking();
    app.update(Msg::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
    draw(&mut app, (100, 24));
    assert!(app.bar.is_empty());
    app.update(Msg::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)));
    assert_eq!(app.focus, Focus::Sessions);
    assert_eq!(app.bar_focus, None);
}

#[test]
fn a_form_keeps_tab_for_its_fields() {
    let mut app = asking();
    // The new-session dialog, by its button.
    press(&mut app, KeyCode::Tab);
    press(&mut app, KeyCode::Tab);
    press(&mut app, KeyCode::Enter);
    // Tab moves the pickers' cursor, then the form's fields.
    let dialog = app.compose.dialog.as_ref().expect("the new-session dialog");
    let selected = dialog.selected;
    press(&mut app, KeyCode::Tab);
    assert_eq!(app.bar_focus, None);
    assert_ne!(
        app.compose.dialog.as_ref().map(|d| d.selected),
        Some(selected)
    );
    press(&mut app, KeyCode::BackTab);
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Enter);
    let field = app.compose.dialog.as_ref().map(|d| d.field);
    press(&mut app, KeyCode::Tab);
    assert_eq!(app.bar_focus, None);
    assert_ne!(app.compose.dialog.as_ref().map(|d| d.field), field);
    press(&mut app, KeyCode::Esc);
    assert!(app.compose.dialog.is_none());
}

#[test]
fn every_dialog_moves_jumps_and_closes_with_gesture_keys() {
    let mut app = asking();

    // The help pages and jumps instead of closing.
    app.help = true;
    press(&mut app, KeyCode::End);
    assert!(app.help);
    assert!(app.help_scroll > 0);
    press(&mut app, KeyCode::Home);
    assert_eq!(app.help_scroll, 0);
    press(&mut app, KeyCode::PageDown);
    press(&mut app, KeyCode::PageUp);
    assert!(app.help);
    press(&mut app, KeyCode::Esc);
    assert!(!app.help);

    // The machines panel: its buttons are a Tab away; Esc closes it.
    let machines = app.bar.len() - 1;
    for _ in 0..=machines {
        press(&mut app, KeyCode::Tab);
    }
    press(&mut app, KeyCode::Enter);
    assert!(app.machine_panel.is_some());
    press(&mut app, KeyCode::End);
    press(&mut app, KeyCode::Home);
    press(&mut app, KeyCode::Backspace);
    assert!(app.machine_panel.is_none());

    // The accounts screen, from the bar too.
    for _ in 0..2 {
        press(&mut app, KeyCode::BackTab);
    }
    press(&mut app, KeyCode::Enter);
    assert!(app.account_screen.is_some());
    press(&mut app, KeyCode::PageDown);
    press(&mut app, KeyCode::PageUp);
    press(&mut app, KeyCode::Esc);
    assert!(app.account_screen.is_none());
}

#[test]
fn the_composer_pages_the_transcript() {
    let mut app = fake::with_prs();
    draw(&mut app, PHONE);
    press(&mut app, KeyCode::Enter);
    app.compose(crate::compose::Act::Write);
    assert_eq!(app.focus, Focus::Composer);
    app.scroll.total = 100;
    app.scroll.height = 10;
    app.update(Msg::Key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE)));
    assert_eq!(app.scroll.first_line(), 81);
    assert_eq!(app.focus, Focus::Composer);
}

#[test]
fn the_machines_panel_jumps_with_home_and_end() {
    let mut app = asking();
    let mut machines = app.machines.clone();
    machines.push(fake::machine("h2", "other", &[]));
    app.update(Msg::Machines(machines));
    app.act(crate::action::Action::OpenMachines);
    let chosen = |app: &App| {
        app.machine_panel
            .as_ref()
            .and_then(|p| p.selected(&app.machines))
    };
    press(&mut app, KeyCode::End);
    assert_eq!(chosen(&app), Some(1));
    press(&mut app, KeyCode::Home);
    assert_eq!(chosen(&app), Some(0));
    press(&mut app, KeyCode::PageDown);
    assert_eq!(chosen(&app), Some(1));
}
