//! What a finger reaches: the header over every screen, and on a narrow screen the bar of
//! buttons over the status line. Each button shows its key too, and its tap reaches one row
//! further than it is drawn: into the border under the header, the status line under the bar.

use herder_protocol::{ApprovalDecision, SessionStatus};
use ratatui::Frame;
use ratatui::crossterm::event::KeyCode;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::action::Action;
use crate::app::{App, Focus};
use crate::compose::Act;
use crate::inbox::{InboxAction, What};
use crate::mouse::{self, Click, Hits};

/// Most characters of a question's choice its button shows.
const CHOICE: usize = 10;

/// A button: its key, what it does in a word or two, and its tap.
struct Button {
    key: String,
    label: String,
    click: Click,
}

fn button(key: &str, label: &str, click: Click) -> Button {
    Button {
        key: key.to_owned(),
        label: label.to_owned(),
        click,
    }
}

/// `area` and the row under it.
fn and_below(area: Rect) -> Rect {
    Rect {
        height: area.height + 1,
        ..area
    }
}

/// The header: `‹ back` where there is somewhere to go back to, the machine and session in
/// view, how many requests wait in the inbox, and `+` for a new session. While a dialog is
/// open, only `‹ back` takes taps: it closes the dialog.
pub(super) fn header(frame: &mut Frame, area: Rect, app: &App, narrow: bool, hits: &mut Hits) {
    let dialog = app.dialog_open();
    let back = dialog || (narrow && app.focus != Focus::Sessions);
    let back_label = if back { " ‹ back " } else { "" };
    let waiting = app.waiting().len();
    let inbox = format!(" inbox {waiting} ");
    let new = " + ";
    let width = |text: &str| u16::try_from(text.chars().count()).unwrap_or(u16::MAX);
    let [back_area, title_area, inbox_area, new_area] = Layout::horizontal([
        Constraint::Length(width(back_label)),
        Constraint::Fill(1),
        Constraint::Length(width(&inbox)),
        Constraint::Length(width(new)),
    ])
    .areas(area);

    let button = Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD);
    frame.render_widget(Line::styled(back_label, button), back_area);
    frame.render_widget(Paragraph::new(title(app, narrow)), title_area);
    let inbox_style = if waiting > 0 {
        Style::new().fg(Color::Magenta).add_modifier(Modifier::BOLD)
    } else {
        super::dim()
    };
    frame.render_widget(Line::styled(inbox, inbox_style), inbox_area);
    frame.render_widget(Line::styled(new, button), new_area);

    if back {
        hits.click(and_below(back_area), mouse::key(KeyCode::Esc));
    }
    if dialog {
        return;
    }
    hits.click(and_below(title_area), Click::Open);
    hits.click(
        and_below(inbox_area),
        Click::Act(Action::Inbox(InboxAction::Toggle)),
    );
    hits.click(
        and_below(new_area),
        Click::Act(Action::Compose(Act::NewSession)),
    );
}

/// The machine and the session in view, else the brand.
fn title(app: &App, narrow: bool) -> Line<'static> {
    let Some((key, session)) = app.open.as_ref().zip(app.open_session()) else {
        return Line::styled(" herder", super::bold());
    };
    let machine = app
        .machines
        .iter()
        .find(|machine| machine.host_id == key.host_id)
        .map_or("", |machine| machine.name.as_str());
    let name = if narrow {
        session.short_title()
    } else {
        session.title()
    };
    Line::from(vec![
        Span::styled(format!(" {machine} › "), super::dim()),
        Span::styled(name, super::bold()),
    ])
}

/// The bar of buttons for what can be done now, as many as fit, the most pressing first.
pub(super) fn bar(frame: &mut Frame, area: Rect, app: &App, hits: &mut Hits) {
    let mut spans = Vec::new();
    let mut x = area.x;
    let key_style = Style::new()
        .fg(Color::Yellow)
        .bg(Color::DarkGray)
        .add_modifier(Modifier::BOLD);
    let label_style = Style::new().fg(Color::White).bg(Color::DarkGray);
    for button in buttons(app) {
        let key = format!(" {} ", button.key);
        let label = format!("{} ", button.label);
        let width = u16::try_from(key.chars().count() + label.chars().count()).unwrap_or(u16::MAX);
        if x + width > area.right() {
            break;
        }
        // The gap after a button is its own, so every spot of the bar taps something.
        hits.click(
            and_below(Rect::new(x, area.y, (width + 1).min(area.right() - x), 1)),
            button.click,
        );
        spans.extend([
            Span::styled(key, key_style),
            Span::styled(label, label_style),
            Span::raw(" "),
        ]);
        x += width + 1;
    }
    frame.render_widget(Line::from(spans), area);
}

/// The buttons for what has the keys now.
fn buttons(app: &App) -> Vec<Button> {
    let enter = || mouse::key(KeyCode::Enter);
    let esc = || button("esc", "close", mouse::key(KeyCode::Esc));
    let char = |c| mouse::key(KeyCode::Char(c));
    if app.help {
        return vec![esc()];
    }
    if app.terminals.is_some() {
        return vec![button("⏎", "attach", enter()), esc()];
    }
    if app.switch.is_some() {
        return vec![button("⏎", "switch", enter()), esc()];
    }
    if app.dialog_open() {
        return vec![button("⏎", "ok", enter()), esc()];
    }
    match app.focus {
        Focus::Sessions => vec![
            button("⏎", "open", enter()),
            button("n", "new", char('n')),
            button("s", "switch", char('s')),
            button("t", "term", char('t')),
            button(":", "cmd", char(':')),
        ],
        Focus::Transcript | Focus::Composer => session_buttons(app),
        Focus::Inbox => inbox_buttons(app),
        Focus::Prs => vec![
            button("⏎", "open", enter()),
            button("x", "unlink", char('x')),
            button("L", "link", char('L')),
        ],
        Focus::AllPrs => vec![
            button("⏎", "open", enter()),
            button("l", "session", char('l')),
            button("x", "unlink", char('x')),
        ],
    }
}

/// The open session's buttons: answers to what it asks, then writing, stopping, switching and
/// a terminal. They act directly, so they work while the composer has the keys too.
fn session_buttons(app: &App) -> Vec<Button> {
    let Some(session) = app.open_session() else {
        return Vec::new();
    };
    let act = |act| Click::Act(Action::Compose(act));
    let mut buttons = Vec::new();
    if !session.approvals.is_empty() {
        buttons.push(button(
            "y",
            "allow",
            act(Act::Approve(ApprovalDecision::Allow)),
        ));
        buttons.push(button(
            "n",
            "deny",
            act(Act::Approve(ApprovalDecision::Deny)),
        ));
    } else if let Some(question) = session.questions.first() {
        buttons.extend(choices(&question.choices, |at| act(Act::Choose(at))));
    }
    if app.focus == Focus::Composer {
        buttons.push(button("⏎", "send", act(Act::Submit)));
    } else if session.status != SessionStatus::Archived {
        buttons.push(button("i", "write", act(Act::Write)));
    }
    if session.turn.is_some() {
        buttons.push(button("^c", "stop", act(Act::CtrlC)));
    }
    buttons.push(button("s", "switch", Click::Act(Action::OpenSwitch)));
    buttons.push(button("t", "term", Click::Act(Action::Terminals)));
    buttons
}

/// The inbox's buttons: answers to the selected request, then opening its session.
fn inbox_buttons(app: &App) -> Vec<Button> {
    let char = |c| mouse::key(KeyCode::Char(c));
    if app.inbox.answer.is_some() {
        return vec![
            button("⏎", "send", mouse::key(KeyCode::Enter)),
            button("esc", "cancel", mouse::key(KeyCode::Esc)),
        ];
    }
    let mut buttons = Vec::new();
    match app.selected_request().map(|waiting| waiting.what) {
        Some(What::Approval(_)) => {
            buttons.push(button("y", "allow", char('y')));
            buttons.push(button("n", "deny", char('n')));
        }
        Some(What::Question(question)) => {
            buttons.extend(choices(&question.choices, |at| {
                Click::Act(Action::Inbox(InboxAction::Choose(at)))
            }));
            buttons.push(button("⏎", "answer", mouse::key(KeyCode::Enter)));
        }
        None => return buttons,
    }
    buttons.push(button("l", "open", char('l')));
    buttons
}

/// A button per choice of a question, up to the nine digits pick.
fn choices(choices: &[String], click: impl Fn(u32) -> Click) -> Vec<Button> {
    (0..9u32)
        .zip(choices)
        .map(|(at, choice)| Button {
            key: (at + 1).to_string(),
            label: super::sessions::clip(choice, CHOICE),
            click: click(at),
        })
        .collect()
}
