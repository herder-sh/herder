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
use crate::keys::{self, LEADER};
use crate::machines::PanelEdit;
use crate::mouse::{self, Click, Hits};
use crate::nav::Mode;
use crate::secrets::Pick;
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
    // Help is every bar's last hint, which the bar never leaves out; where `?` types, the
    // leader's `?` opens it.
    let help = tap("?", "help", char('?'));
    let typing_help = act("ctrl+x ?", "help", Action::ToggleHelp);
    let running = app.open_session().is_some_and(|s| s.turn.is_some());
    let stop = act("ctrl+c", "stop", Action::Compose(Act::CtrlC));
    let secrets = app
        .machine_panel
        .as_ref()
        .and_then(|panel| panel.secrets.as_ref())
        .filter(|secrets| secrets.dialog.is_none());
    if let Some(secrets) = secrets
        && app.mode().is_none()
    {
        let mut hints = vec![hint("j/k", "move"), tap("n", "new set", char('n'))];
        if let Some(pick) = secrets.pick() {
            if matches!(pick, Pick::Secret { .. }) {
                hints.push(tap("e", "replace", char('e')));
            }
            hints.extend([
                tap("a", "add secret", char('a')),
                tap("s", "hosts", char('s')),
                tap("d", "delete", char('d')),
            ]);
        }
        hints.extend([
            tap("r", "reload", char('r')),
            tap("esc", "back", KeyCode::Esc),
            help,
        ]);
        return hints;
    }
    match app.mode() {
        // The fleet and the accounts are views, whose keys the bar shows.
        None if app.machine_panel.as_ref().is_some_and(|panel| {
            panel.add.is_none()
                && panel.account.is_none()
                && panel.backup.is_none()
                && panel.secrets.is_none()
        }) =>
        {
            match app
                .machine_panel
                .as_ref()
                .and_then(|panel| panel.edit.as_ref())
            {
                Some(PanelEdit::Rename(_)) => vec![
                    tap("enter", "save on this device", KeyCode::Enter),
                    tap("esc", "cancel", KeyCode::Esc),
                ],
                Some(PanelEdit::Forget) => {
                    vec![tap("y", "forget", char('y')), tap("n", "keep", char('n'))]
                }
                None => vec![
                    hint("j/k", "move"),
                    tap("a", "add machine", char('a')),
                    tap("n", "add account", char('n')),
                    tap("e", "rename", char('e')),
                    // A vault backs nothing up; its secrets are managed there.
                    if app.on_vault() {
                        tap("s", "secrets", char('s'))
                    } else {
                        tap("b", "backup", char('b'))
                    },
                    tap("d", "forget", char('d')),
                    tap("r", "reconnect", char('r')),
                    tap("esc", "back", KeyCode::Esc),
                    help,
                ],
            }
        }
        None if app
            .account_screen
            .as_ref()
            .is_some_and(|screen| screen.adding.is_none()) =>
        {
            let mut hints = vec![hint("j/k", "move"), tap("n", "add account", char('n'))];
            if app.on_account() {
                hints.push(tap("l", "log in again", char('l')));
            }
            hints.extend([
                tap("r", "reconnect", char('r')),
                tap("esc", "back", KeyCode::Esc),
                help,
            ]);
            hints
        }
        // A dialog shows its own keys; this is its tap to close.
        None => vec![tap("esc", "close", KeyCode::Esc)],
        Some(Mode::Leader) => vec![tap("esc", "cancel", KeyCode::Esc), help],
        // An approval takes the prompt's place, and its keys.
        Some(Mode::Prompt) if app.open_session().is_some_and(|s| !s.approvals.is_empty()) => {
            let mut hints = vec![
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
                hint("←/→", "choose"),
                tap("enter", "confirm", KeyCode::Enter),
                tap("f", "full", char('f')),
                tap("esc", "navigate", KeyCode::Esc),
            ];
            if running {
                hints.push(stop);
            }
            hints.push(typing_help);
            hints
        }
        Some(Mode::Prompt) => {
            let mut hints = vec![
                tap("enter", "send", KeyCode::Enter),
                (
                    Hint::new("alt+enter", "newline"),
                    Some(Click::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT))),
                ),
                hint("/", "commands"),
                hint("@", "mention"),
                tap("esc", "navigate", KeyCode::Esc),
            ];
            if running {
                hints.push(stop);
            }
            hints.extend([leader, typing_help]);
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
            hints.extend([tap("esc", "navigate", KeyCode::Esc), leader, help]);
            hints
        }
        Some(Mode::Navigate) if app.machines.is_empty() => vec![
            tap("a", "pair a machine", char('a')),
            tap("q", "quit", char('q')),
            help,
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
                    hint("[ ]", "items"),
                    tap("e", "expand", char('e')),
                    tap("c", "copy", char('c')),
                ];
                if app.open_session().is_some_and(has_images) {
                    hints.extend([
                        tap("o", "open images", char('o')),
                        tap("w", "save", char('w')),
                    ]);
                }
                hints.push(tap("esc", "sidebar", KeyCode::Esc));
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
                help,
            ],
            Focus::Prs => vec![
                hint("j/k", "move"),
                tap("enter", "browser", KeyCode::Enter),
                tap("x", "unlink", char('x')),
                tap("L", "link", char('L')),
                tap("esc", "chat", KeyCode::Esc),
                help,
            ],
            Focus::AllPrs => vec![
                hint("j/k", "move"),
                tap("enter", "browser", KeyCode::Enter),
                tap("l", "session", char('l')),
                tap("x", "unlink", char('x')),
                tap("esc", "back", KeyCode::Esc),
                help,
            ],
            Focus::Inbox if app.inbox.answer.is_some() => vec![
                tap("enter", "send", KeyCode::Enter),
                tap("esc", "cancel", KeyCode::Esc),
                typing_help,
            ],
            Focus::Board => vec![
                hint("j/k", "move"),
                tap("enter", "open", KeyCode::Enter),
                tap("esc", "back", KeyCode::Esc),
                help,
            ],
            Focus::Inbox => vec![
                hint("j/k", "move"),
                hint("1-9", "pick"),
                hint("y/n", "allow/deny"),
                tap("enter", "answer", KeyCode::Enter),
                tap("l", "open", char('l')),
                tap("esc", "back", KeyCode::Esc),
                help,
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
    let mut entries: Vec<(String, &str)> = LEADER
        .iter()
        .map(|bind| (keys::names(bind.keys), bind.does))
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

/// Whether a prompt of `session` carries images.
fn has_images(session: &crate::session::Session) -> bool {
    session.entries.iter().any(|entry| {
        matches!(entry, crate::session::Entry::Item(item)
            if matches!(&item.body, herder_protocol::ItemBody::UserMessage { attachments, .. }
                if !attachments.is_empty()))
    })
}
