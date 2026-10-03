//! Taps and wheel steps on every view, on a phone-sized screen and a desktop one: each test
//! draws the screen, finds what a finger would aim at by its text, and taps or swipes there.

use herder_protocol::{
    AccountId, ApprovalDecision, CommandBody, EventBody, HostId, ItemBody, Provider,
};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};

use crate::app::{App, Effect, Focus, Msg};
use crate::compose::Origin;
use crate::fake::{self, added, key, update};
use crate::glyphs::{self, Glyphs};

/// A phone over SSH, and a desktop terminal.
const SIZES: [(u16, u16); 2] = [(45, 40), (120, 40)];

fn press(app: &mut App, code: KeyCode) -> Vec<Effect> {
    app.update(Msg::Key(KeyEvent::new(code, KeyModifiers::NONE)))
}

fn draw(app: &mut App, (width, height): (u16, u16)) -> Terminal<TestBackend> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| crate::views::draw(frame, app))
        .unwrap();
    terminal
}

/// Where `text` is on the screen, top to bottom.
fn spots(app: &mut App, size: (u16, u16), text: &str) -> Vec<(u16, u16)> {
    let terminal = draw(app, size);
    let buffer = terminal.backend().buffer();
    // As the screen shows it: with ASCII glyphs on a phone.
    let shown = match Glyphs::for_width(app.glyphs, size.0 - 1) {
        Glyphs::Ascii => glyphs::folded(text),
        Glyphs::Unicode => text.to_owned(),
    };
    let wanted: Vec<String> = shown.chars().map(String::from).collect();
    let mut found = Vec::new();
    for y in 0..size.1 {
        let row: Vec<&str> = (0..size.0).map(|x| buffer[(x, y)].symbol()).collect();
        for x in 0..row.len().saturating_sub(wanted.len() - 1) {
            if row[x..x + wanted.len()]
                .iter()
                .zip(&wanted)
                .all(|(a, b)| a == b)
            {
                found.push((u16::try_from(x).unwrap(), y));
            }
        }
    }
    assert!(
        !found.is_empty(),
        "no {text:?} at {size:?}:\n{}",
        terminal.backend()
    );
    found
}

fn mouse(app: &mut App, kind: MouseEventKind, (column, row): (u16, u16)) -> Vec<Effect> {
    app.update(Msg::Mouse(MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }))
}

/// Taps at `at`: a press and a release.
fn tap_at(app: &mut App, at: (u16, u16)) -> Vec<Effect> {
    assert_eq!(mouse(app, MouseEventKind::Down(MouseButton::Left), at), []);
    mouse(app, MouseEventKind::Up(MouseButton::Left), at)
}

/// Taps the first `text` on the screen.
fn tap(app: &mut App, size: (u16, u16), text: &str) -> Vec<Effect> {
    let at = spots(app, size, text)[0];
    tap_at(app, at)
}

/// Taps the last `text` on the screen: the one in a list rather than in the header.
fn tap_last(app: &mut App, size: (u16, u16), text: &str) -> Vec<Effect> {
    let at = *spots(app, size, text).last().unwrap();
    tap_at(app, at)
}

/// Turns the wheel `steps` notches over the first `text`, down for positive ones.
fn wheel(app: &mut App, size: (u16, u16), text: &str, steps: i32) -> Vec<Effect> {
    let at = spots(app, size, text)[0];
    let kind = if steps < 0 {
        MouseEventKind::ScrollUp
    } else {
        MouseEventKind::ScrollDown
    };
    (0..steps.abs())
        .flat_map(|_| mouse(app, kind, at))
        .collect()
}

fn on(session: &str, command: CommandBody) -> Effect {
    Effect::Send {
        host_id: HostId::new("h1"),
        command,
        origin: Origin::Session(key("h1", session)),
    }
}

/// `s2` of [`fake::tree`] open, with `bodies` fed to it from seq 3.
fn open_s2(bodies: Vec<EventBody>) -> App {
    let mut app = fake::tree();
    fake::feed(&mut app, "h1", "s2", update("s2", 3, bodies, Vec::new()));
    press(&mut app, KeyCode::Enter);
    app
}

#[test]
fn a_tap_opens_a_session_and_back_returns_to_the_list() {
    for size in SIZES {
        let mut app = fake::tree();
        tap_last(&mut app, size, "fix-login");
        assert_eq!(app.open, Some(key("h1", "s1")), "{size:?}");
        assert_eq!(app.focus, Focus::Transcript);
        if size.0 < crate::views::NARROW {
            tap(&mut app, size, "‹ back");
            assert_eq!(app.focus, Focus::Sessions);
        } else {
            press(&mut app, KeyCode::Esc);
        }
        // The title in the header shows the open session again.
        tap(&mut app, size, "fix-login");
        assert_eq!(app.focus, Focus::Transcript);
    }
}

#[test]
fn a_tap_on_a_machine_selects_it_without_opening_anything() {
    for size in SIZES {
        let mut app = fake::tree();
        tap(&mut app, size, "● box");
        assert_eq!(
            app.selected(),
            Some(crate::app::Row::Machine(HostId::new("h1")))
        );
        assert_eq!(app.open, None);
    }
}

#[test]
fn the_wheel_scrolls_the_view_under_the_pointer() {
    for size in SIZES {
        let lines: Vec<EventBody> = (0..60)
            .map(|at| {
                added(
                    &format!("i{at}"),
                    ItemBody::UserMessage {
                        text: format!("message {at}"),
                    },
                )
            })
            .collect();
        let mut app = open_s2(lines);
        draw(&mut app, size);
        assert_eq!(app.scroll.top, None, "follows the end");
        wheel(&mut app, size, "message 59", -2);
        let first = app.scroll.first_line();
        assert_eq!(app.scroll.top, Some(first));
        assert_eq!(first, app.scroll.total - app.scroll.height - 6, "{size:?}");
        wheel(&mut app, size, "message 5", 2);
        assert_eq!(app.scroll.top, None, "back at the end, it follows again");

        // The session list scrolls its selection, where it shows.
        if size.0 < crate::views::NARROW {
            tap(&mut app, size, "‹ back");
        }
        let before = app.selected();
        wheel(&mut app, size, "fix-login", 1);
        assert_ne!(app.selected(), before, "{size:?}");
    }
}

#[test]
fn a_tap_on_allow_or_deny_answers_the_approval() {
    for size in SIZES {
        for (label, decision) in [
            ("y allow", ApprovalDecision::Allow),
            ("n deny", ApprovalDecision::Deny),
        ] {
            let mut app = open_s2(vec![
                fake::started("turn-1"),
                fake::approval("a1", "Bash: rm -rf target"),
            ]);
            // Also while the composer has the keys, where y would be typed.
            press(&mut app, KeyCode::Char('i'));
            let effects = tap_last(&mut app, size, label);
            let command = CommandBody::AnswerApproval {
                session_id: herder_protocol::SessionId::new("s2"),
                approval_id: herder_protocol::ApprovalId::new("a1"),
                decision,
            };
            assert_eq!(effects, [on("s2", command)], "{size:?} {label}");
        }
    }
}

#[test]
fn a_tap_on_a_choice_answers_the_question() {
    for size in SIZES {
        let mut app = open_s2(vec![
            fake::started("turn-1"),
            fake::question("q1", "Which port?", &["8080", "3000"]),
        ]);
        let effects = tap_last(&mut app, size, "2. 3000");
        let command = CommandBody::AnswerQuestion {
            session_id: herder_protocol::SessionId::new("s2"),
            question_id: herder_protocol::QuestionId::new("q1"),
            answer: herder_protocol::Answer::Choice { index: 1 },
        };
        assert_eq!(effects, [on("s2", command)], "{size:?}");
    }
}

#[test]
fn the_bar_stops_a_turn_and_sends_the_composer_on_a_phone() {
    let size = SIZES[0];
    let mut app = open_s2(vec![fake::started("turn-1")]);
    let interrupt = CommandBody::Interrupt {
        session_id: herder_protocol::SessionId::new("s2"),
    };
    assert_eq!(tap(&mut app, size, "^c stop"), [on("s2", interrupt)]);
    // A tap on the composer writes in it; the bar then sends.
    tap(&mut app, size, "i to write");
    assert_eq!(app.focus, Focus::Composer);
    fake::type_text(&mut app, "go on");
    let effects = tap(&mut app, size, "⏎ send");
    assert!(
        matches!(&effects[..], [Effect::Send { command: CommandBody::SendPrompt { text, .. }, .. }] if text == "go on"),
        "{effects:?}"
    );
}

#[test]
fn the_header_opens_the_inbox_and_new_session() {
    for size in SIZES {
        let mut app = fake::escalated();
        tap(&mut app, size, "inbox 2");
        assert_eq!(app.focus, Focus::Inbox, "{size:?}");
        tap(&mut app, size, "inbox 2");
        assert_ne!(app.focus, Focus::Inbox);
        tap(&mut app, size, " + ");
        assert!(app.compose.dialog.is_some(), "{size:?}");
        // The dialog covers the screen: the list under it takes no taps.
        tap(&mut app, size, "● box");
        assert_eq!(app.open, None);
        tap(&mut app, size, "‹ back");
        assert!(app.compose.dialog.is_none());
    }
}

#[test]
fn the_inbox_selects_by_tap_and_wheel_and_answers_on_a_phone() {
    for size in SIZES {
        let mut app = fake::escalated();
        press(&mut app, KeyCode::Char('I'));
        // Newest first: the question, then the approval.
        wheel(&mut app, size, "Which port", 1);
        assert_eq!(app.inbox_index(&app.waiting()), 1, "{size:?}");
        tap_last(&mut app, size, "Which port");
        assert_eq!(app.inbox_index(&app.waiting()), 0);
        if size.0 < crate::views::NARROW {
            let effects = tap(&mut app, size, "1 8080");
            assert!(
                matches!(
                    &effects[..],
                    [Effect::Send {
                        command: CommandBody::AnswerQuestion { .. },
                        ..
                    }]
                ),
                "{effects:?}"
            );
            tap_last(&mut app, size, "cargo publish");
            let effects = tap(&mut app, size, "y allow");
            assert!(
                matches!(
                    &effects[..],
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
        }
        // A tap on the selected request opens its session.
        tap_last(&mut app, size, "cargo publish");
        tap_last(&mut app, size, "cargo publish");
        assert_eq!(app.open, Some(key("h1", "s1")), "{size:?}");
        assert_eq!(app.focus, Focus::Transcript);
    }
}

#[test]
fn pull_requests_select_by_tap_and_wheel_and_open_on_a_second_tap() {
    for size in SIZES {
        let mut app = fake::with_prs();
        press(&mut app, KeyCode::Char('P'));
        assert_eq!(app.focus, Focus::AllPrs);
        tap_last(&mut app, size, "#9");
        assert_eq!(app.pr_index(), 1, "{size:?}");
        wheel(&mut app, size, "Document the health", 1);
        assert_eq!(app.pr_index(), 2);
        wheel(&mut app, size, "Document the health", -2);
        assert_eq!(app.pr_index(), 0);
        tap_last(&mut app, size, "#7");
        let effects = tap_last(&mut app, size, "#7");
        assert!(
            matches!(&effects[..], [Effect::OpenUrl(url)] if url.ends_with("/pull/7")),
            "{effects:?}"
        );

        // The strip over a transcript takes the keys on a tap.
        press(&mut app, KeyCode::Esc);
        let mut app = fake::with_prs();
        press(&mut app, KeyCode::Enter);
        wheel(&mut app, size, "Add a health endpoint", 1);
        assert_eq!(app.prs.strip, 1, "{size:?}");
        tap(&mut app, size, "Add a health endpoint");
        assert_eq!(app.focus, Focus::Prs, "{size:?}");
        assert_eq!(app.pr_index(), 0);
    }
}

/// [`fake::tree`] with accounts `Main` and `Work` on `box`, and a second machine.
fn with_accounts() -> App {
    let mut app = fake::tree();
    let mut machines = app.machines.clone();
    machines[0].accounts = vec![
        fake::account("claude-main", "Main"),
        fake::account("claude-work", "Work"),
    ];
    let mut codex = fake::account("codex", "Codex");
    codex.provider = Provider::Codex;
    machines[0].accounts.push(codex);
    machines.push(fake::machine("h2", "laptop", &[]));
    app.update(Msg::Machines(machines));
    app
}

#[test]
fn the_accounts_screen_selects_by_tap_and_wheel_and_closes_by_back() {
    for size in SIZES {
        let mut app = with_accounts();
        press(&mut app, KeyCode::Char('A'));
        let chosen = |app: &App| {
            let rows = crate::account_screen::rows(&app.machines);
            let screen = app.account_screen.as_ref().unwrap();
            screen.selected(&rows).map(|at| rows[at].clone())
        };
        let work =
            crate::account_screen::Pick::Account(HostId::new("h1"), AccountId::new("claude-work"));
        wheel(&mut app, size, "Main", 1);
        assert_eq!(chosen(&app), Some(work.clone()), "{size:?}");
        tap(&mut app, size, "laptop");
        assert_eq!(
            chosen(&app),
            Some(crate::account_screen::Pick::Machine(HostId::new("h2")))
        );
        tap(&mut app, size, "‹ back");
        assert!(app.account_screen.is_none());
    }
}

#[test]
fn the_machines_panel_selects_by_tap() {
    for size in SIZES {
        let mut app = with_accounts();
        press(&mut app, KeyCode::Char('m'));
        tap(&mut app, size, "0 sessions");
        let panel = app.machine_panel.as_ref().unwrap();
        assert_eq!(panel.chosen, Some(HostId::new("h2")), "{size:?}");
        wheel(&mut app, size, "0 sessions", -1);
        let panel = app.machine_panel.as_ref().unwrap();
        assert_eq!(panel.selected(&app.machines), Some(0));
        tap(&mut app, size, "‹ back");
        assert!(app.machine_panel.is_none());
    }
}

#[test]
fn the_switch_dialog_picks_an_account_by_tap_and_switches_on_a_second() {
    for size in SIZES {
        let mut app = with_accounts();
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Char('s'));
        tap(&mut app, size, "Work");
        assert_eq!(app.switch.as_ref().unwrap().selected, 1, "{size:?}");
        let effects = tap(&mut app, size, "Work");
        assert!(
            matches!(
                &effects[..],
                [Effect::Send {
                    command: CommandBody::SwitchAccount { .. },
                    ..
                }]
            ),
            "{effects:?}"
        );
    }
}

#[test]
fn recover_opens_by_a_tap_on_the_open_offline_session() {
    for size in SIZES {
        let mut app = fake::vault();
        tap_last(&mut app, size, "docs");
        assert_eq!(app.open, Some(key("v", "s2")), "{size:?}");
        tap(&mut app, size, "R recover");
        assert!(app.recover.is_some(), "{size:?}");
        tap_last(&mut app, size, "devbox");
        assert_eq!(app.recover.as_ref().map(|r| r.selected), Some(0));
        if size.0 < crate::views::NARROW {
            tap(&mut app, size, "esc close");
        } else {
            press(&mut app, KeyCode::Esc);
        }
        assert!(app.recover.is_none(), "{size:?}");
    }
}

#[test]
fn the_terminal_picker_attaches_on_a_second_tap() {
    for size in SIZES {
        let mut app = crate::terminal::app_tests::with_terminals();
        press(&mut app, KeyCode::Char('t'));
        tap_last(&mut app, size, "terminal t3");
        assert_eq!(app.terminals.as_ref().unwrap().selected, 2, "{size:?}");
        wheel(&mut app, size, "terminal t3", -1);
        assert_eq!(app.terminals.as_ref().unwrap().selected, 1);
        let effects = tap_last(&mut app, size, "terminal t1");
        assert!(
            matches!(&effects[..], [Effect::AttachTerminal { .. }]),
            "{effects:?}"
        );
    }
}

#[test]
fn the_help_scrolls_by_wheel_and_closes_on_a_tap() {
    for size in SIZES {
        let mut app = fake::tree();
        press(&mut app, KeyCode::Char('?'));
        wheel(&mut app, size, "keys", 1);
        assert!(app.help_scroll > 0, "{size:?}");
        tap(&mut app, size, "keys");
        assert!(!app.help, "{size:?}");
    }
}

#[test]
fn a_press_that_slides_off_is_no_tap() {
    let size = SIZES[1];
    let mut app = fake::tree();
    let from = *spots(&mut app, size, "fix-login").last().unwrap();
    let to = spots(&mut app, size, "box").last().copied().unwrap();
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), from);
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), to);
    assert_eq!(app.open, None);
}

#[test]
fn mouse_off_saves_the_setting_and_ignores_the_mouse() {
    let size = SIZES[0];
    let mut app = fake::tree();
    press(&mut app, KeyCode::Char(':'));
    fake::type_text(&mut app, "mouse off");
    assert_eq!(
        press(&mut app, KeyCode::Enter),
        [Effect::Mouse(false), Effect::Save]
    );
    assert!(!app.mouse);
    tap_last(&mut app, size, "fix-login");
    assert_eq!(app.open, None);

    press(&mut app, KeyCode::Char(':'));
    fake::type_text(&mut app, "mouse maybe");
    assert_eq!(press(&mut app, KeyCode::Enter), []);
    let palette = app.compose.palette.as_ref().unwrap();
    assert_eq!(palette.error.as_deref(), Some("usage: mouse on|off"));
    press(&mut app, KeyCode::Esc);
    press(&mut app, KeyCode::Char(':'));
    fake::type_text(&mut app, "mouse on");
    assert_eq!(
        press(&mut app, KeyCode::Enter),
        [Effect::Mouse(true), Effect::Save]
    );
    tap_last(&mut app, size, "fix-login");
    assert_eq!(app.open, Some(key("h1", "s1")));
}
