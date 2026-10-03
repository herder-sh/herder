//! The desktop's bottom bar, a [`ModeBar`], as Herdr's mode bar: the mode badge, what waits
//! on the user or a notice, the keys that work right now (each a tap), and each machine's
//! connection at the right end. While the leader waits, a popup over the bar lists its keys.
//!
//! ```text
//!  PROMPT  enter send  alt+enter newline  esc navigate  ctrl+x leader        ● box  ● m2
//! ```

use herder_client_core::ConnectionState;
use herder_protocol::ApprovalDecision;
use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};

use crate::action::Action;
use crate::app::{App, Focus};
use crate::compose::Act;
use crate::mouse::{self, Click, Hits};
use crate::nav::{LEADER_KEYS, Mode};
use crate::ui::badge;
use crate::ui::hints::{Hint, ModeBar};
use crate::ui::state::{self, State};
use crate::ui::{GAP, Ui};

/// The bar in `area`'s first row.
pub(super) fn draw(frame: &mut Frame, area: Rect, app: &App, hits: &mut Hits) {
    let ui = app.ui();
    let theme = ui.theme;
    let mut lead = Vec::new();
    if let Some(mode) = app.mode() {
        let color = match mode {
            Mode::Prompt => theme.primary,
            Mode::Navigate => theme.secondary,
            Mode::Approval => theme.attention,
            Mode::Leader => theme.accent,
        };
        lead.push(badge::solid(ui, mode.label(), color));
    }
    let waiting = app.waiting().len();
    if let Some(notice) = &app.notice {
        lead.push(Span::raw(" ".repeat(GAP)));
        lead.push(Span::styled(notice.clone(), Style::new().fg(theme.warning)));
    } else if waiting > 0 && app.focus != Focus::Inbox {
        lead.push(Span::raw(" ".repeat(GAP)));
        lead.push(Span::styled(
            format!("{} {waiting} waiting", ui.glyphs.state(State::NeedsYou)),
            state::style(ui, State::NeedsYou),
        ));
    }
    let hints = hints(app);
    let shown: Vec<Hint> = hints.iter().map(|(hint, _)| hint.clone()).collect();
    let placed = ModeBar::new(ui, &shown)
        .lead(Line::from(lead))
        .right(connections(app, ui))
        .render(area, frame.buffer_mut());
    for (at, rect) in placed {
        if let Some(click) = &hints[at].1 {
            hits.click(rect, click.clone());
        }
    }
}

/// Each machine's connection: `● box  ◌ m2  ✗ vault`.
pub(super) fn connections(app: &App, ui: Ui) -> Line<'static> {
    let mut spans = Vec::new();
    for machine in &app.machines {
        let (mark, color) = match &machine.connection {
            ConnectionState::Connected => (ui.glyphs.connected, ui.theme.success),
            ConnectionState::Connecting => (ui.glyphs.connecting, ui.theme.warning),
            ConnectionState::Disconnected { .. } => (ui.glyphs.disconnected, ui.theme.error),
        };
        if !spans.is_empty() {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(mark, Style::new().fg(color)));
        spans.push(Span::styled(format!(" {}", machine.name), ui.text()));
    }
    Line::from(spans)
}

/// The keys that work now, for the mode and what has focus; each with its tap where one key
/// does it.
fn hints(app: &App) -> Vec<(Hint, Option<Click>)> {
    let hint = |key: &'static str, label: &'static str| (Hint::new(key, label), None);
    let tap = |key: &'static str, label: &'static str, code: KeyCode| {
        (Hint::new(key, label), Some(mouse::key(code)))
    };
    let act = |key: &'static str, label: &'static str, action: Action| {
        (Hint::new(key, label), Some(Click::Act(action)))
    };
    let char = |c: char| KeyCode::Char(c);
    let leader = act("ctrl+x", "leader", Action::Leader);
    let help = tap("?", "help", char('?'));
    let running = app.open_session().is_some_and(|s| s.turn.is_some());
    let stop = act("ctrl+c", "stop", Action::Compose(Act::CtrlC));
    match app.mode() {
        // A dialog shows its own keys; this is its tap to close.
        None => vec![tap("esc", "close", KeyCode::Esc)],
        Some(Mode::Leader) => vec![tap("esc", "cancel", KeyCode::Esc)],
        Some(Mode::Prompt) => {
            let mut hints = vec![
                tap("enter", "send", KeyCode::Enter),
                (
                    Hint::new("alt+enter", "newline"),
                    Some(Click::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT))),
                ),
                tap("esc", "navigate", KeyCode::Esc),
            ];
            if running {
                hints.push(stop);
            }
            hints.push(leader);
            hints
        }
        Some(Mode::Approval) => {
            let asks_approval = app.open_session().is_some_and(|s| !s.approvals.is_empty());
            let mut hints = if asks_approval {
                vec![
                    act(
                        "y",
                        "allow",
                        Action::Compose(Act::Approve(ApprovalDecision::Allow)),
                    ),
                    act(
                        "n",
                        "deny",
                        Action::Compose(Act::Approve(ApprovalDecision::Deny)),
                    ),
                ]
            } else {
                vec![hint("1-9", "pick"), tap("i", "type an answer", char('i'))]
            };
            hints.push(tap("esc", "navigate", KeyCode::Esc));
            hints.push(leader);
            hints
        }
        Some(Mode::Navigate) if app.machines.is_empty() => vec![
            tap("a", "pair a machine", char('a')),
            help,
            tap("q", "quit", char('q')),
        ],
        Some(Mode::Navigate) => match app.focus {
            Focus::Sessions => vec![
                hint("j/k", "move"),
                tap("enter", "open", KeyCode::Enter),
                tap("n", "new", char('n')),
                tap("v", "group", char('v')),
                tap("z", "fold", char('z')),
                tap(":", "commands", char(':')),
                leader,
                help,
            ],
            Focus::Transcript | Focus::Composer => {
                let mut hints = vec![
                    tap("i", "write", char('i')),
                    hint("j/k", "scroll"),
                    tap("esc", "sidebar", KeyCode::Esc),
                ];
                if running {
                    hints.push(stop);
                }
                hints.extend([
                    tap("s", "switch", char('s')),
                    tap("p", "prs", char('p')),
                    leader,
                    help,
                ]);
                hints
            }
            Focus::Tasks => vec![
                hint("j/k", "move"),
                tap("enter", "open", KeyCode::Enter),
                tap("esc", "chat", KeyCode::Esc),
                leader,
            ],
            Focus::Prs => vec![
                hint("j/k", "move"),
                tap("enter", "browser", KeyCode::Enter),
                tap("x", "unlink", char('x')),
                tap("L", "link", char('L')),
                tap("esc", "chat", KeyCode::Esc),
            ],
            Focus::AllPrs => vec![
                hint("j/k", "move"),
                tap("enter", "browser", KeyCode::Enter),
                tap("l", "session", char('l')),
                tap("x", "unlink", char('x')),
                tap("esc", "back", KeyCode::Esc),
            ],
            Focus::Inbox if app.inbox.answer.is_some() => vec![
                tap("enter", "send", KeyCode::Enter),
                tap("esc", "cancel", KeyCode::Esc),
            ],
            Focus::Inbox => vec![
                hint("j/k", "move"),
                hint("1-9", "pick"),
                hint("y/n", "allow/deny"),
                tap("enter", "answer", KeyCode::Enter),
                tap("l", "open", char('l')),
                tap("esc", "back", KeyCode::Esc),
            ],
        },
    }
}

/// While the leader waits: a popup over the bottom-left of `area` listing what each key
/// after `ctrl+x` does.
pub(super) fn leader_popup(frame: &mut Frame, area: Rect, app: &App) {
    if app.leader.is_none() {
        return;
    }
    let ui = app.ui();
    let mut entries: Vec<(String, &str)> = LEADER_KEYS
        .iter()
        .map(|(key, label)| (key.to_string(), *label))
        .collect();
    entries.push(("1-9".to_owned(), "attention row"));
    let cell = entries
        .iter()
        .map(|(key, label)| key.chars().count() + 1 + label.len())
        .max()
        .unwrap_or(0)
        + GAP;
    // Columns of entries, as many as fit the popup's width, read down.
    let max_width = usize::from(area.width.saturating_sub(4)).min(4 * cell);
    let columns = (max_width / cell).max(1);
    let rows = entries.len().div_ceil(columns);
    let mut lines = vec![Line::default(); rows];
    for (at, (key, label)) in entries.iter().enumerate() {
        let line = &mut lines[at % rows];
        let pad = cell - (key.chars().count() + 1 + label.len());
        line.spans.push(Span::styled(key.clone(), ui.accent()));
        line.spans.push(Span::styled(
            format!(" {label}{}", " ".repeat(pad)),
            ui.text(),
        ));
    }
    let width = u16::try_from(columns * cell + 2 + 1)
        .unwrap_or(u16::MAX)
        .min(area.width);
    let height = u16::try_from(rows + 2).unwrap_or(u16::MAX).min(area.height);
    // Full width on a phone, so nothing under it peeks out at its side.
    let (x, width) = if area.width < super::NARROW {
        (area.x, area.width)
    } else {
        (area.x + 1, width)
    };
    let popup = Rect::new(x, area.bottom().saturating_sub(height), width, height);
    // Lined in Unicode; in ASCII a menu panel, its title on its first row.
    let block = super::pane(app)
        .border_style(ui.border(true))
        .style(Style::new().bg(ui.theme.background_menu))
        .title(Span::styled(" ctrl+x ", ui.strong()))
        .padding(ratatui::widgets::Padding::horizontal(1));
    frame.render_widget(Clear, popup);
    frame.render_widget(Paragraph::new(lines).block(block), popup);
}
