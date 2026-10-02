//! The switch dialog, over the main screen: the session's machine's accounts with their
//! busiest usage window, the model, and what switching to the chosen account does.

use herder_protocol::Account;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Padding, Paragraph};

use crate::account_screen;
use crate::app::App;
use crate::mouse::{Click, Hits, List as Rows};
use crate::session::Session;
use crate::switch::{self, Kind};

pub(super) fn draw(frame: &mut Frame, area: Rect, app: &App, hits: &mut Hits) {
    let Some(switch) = &app.switch else {
        return;
    };
    let Some(session) = app.sessions.get(&switch.session) else {
        return;
    };
    let accounts = app.accounts_of(&switch.session);
    let accounts_len = accounts.len();
    let current = session
        .account_id
        .as_ref()
        .and_then(|id| account_screen::find(&app.machines, &switch.session.host_id, id));
    let now_on = match (current, &session.account_id) {
        (Some(account), _) => format!("{} ({})", account.label, account.provider.as_str()),
        (None, Some(id)) => id.to_string(),
        (None, None) => "unknown".to_owned(),
    };
    let mut lines = vec![
        Line::from(vec![
            Span::styled("now  ", super::dim()),
            Span::raw(format!("{now_on} · {}", session.model)),
        ]),
        Line::raw(""),
    ];
    if accounts.is_empty() {
        lines.push(Line::styled(
            "This machine has no accounts: A shows them.",
            super::dim(),
        ));
    }
    let label_width = accounts
        .iter()
        .map(|account| account.label.chars().count())
        .max()
        .unwrap_or(0);
    let first_account = lines.len();
    for (at, account) in accounts.iter().enumerate() {
        lines.push(account_line(
            session,
            account,
            at == switch.selected,
            label_width,
        ));
    }
    let chosen = accounts.get(switch.selected);
    let placeholder = match chosen.map(|account| switch::kind(session, account)) {
        Some(Kind::Provider) => "the provider's default".to_owned(),
        _ => format!("keep {}", session.model),
    };
    let label_style = if switch.editing {
        Style::new().fg(Color::Cyan)
    } else {
        super::dim()
    };
    let mut model = vec![
        Span::styled("model  ", label_style),
        Span::raw(switch.model.clone()),
    ];
    if switch.editing {
        model.push(Span::styled("▌", Style::new().fg(Color::Cyan)));
    }
    if switch.model.is_empty() {
        model.push(Span::styled(placeholder, super::dim()));
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(model));
    lines.push(Line::raw(""));
    let width = area.width.saturating_sub(4).min(72);
    // Inside the borders and padding.
    let wrap_at = usize::from(width.saturating_sub(4)).max(8);
    let mut wrapped = |text: &str, style: Style| {
        for part in textwrap::wrap(text, wrap_at) {
            lines.push(Line::styled(part.into_owned(), style));
        }
    };
    if let Some(account) = chosen {
        wrapped(&what(session, account), super::dim());
    }
    if let Some(error) = &switch.error {
        wrapped(error, Style::new().fg(Color::Red));
    }
    let keys = if switch.editing {
        " Enter switch  ⌫ accounts "
    } else if area.width < super::NARROW {
        " Enter switch  m model  ⌫ close "
    } else {
        " Enter switch  j/k account  m model  Esc close "
    };
    let block = Block::bordered()
        .title(Line::styled(
            format!(" switch {} ", session.short_title()),
            super::bold(),
        ))
        .title_bottom(Line::styled(keys, super::dim()).centered())
        .border_style(Style::new().fg(Color::Cyan))
        .padding(Padding::uniform(1));
    let height = u16::try_from(lines.len() + 4).unwrap_or(u16::MAX);
    let area = super::centered(area, width, height);
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(lines).block(block), area);
    let first_account = inner.y + u16::try_from(first_account).unwrap_or(u16::MAX);
    let rows = Rect::new(inner.x, first_account, inner.width, inner.height).intersection(inner);
    hits.list(rows, 0, &vec![1; accounts_len], |at| {
        Some(Click::Row(Rows::Switch, at))
    });
}

/// An account to pick: its label, provider and busiest window, marked if the session is on it.
fn account_line(
    session: &Session,
    account: &Account,
    chosen: bool,
    label_width: usize,
) -> Line<'static> {
    let mark = if switch::kind(session, account) == Kind::Current {
        "• "
    } else {
        "  "
    };
    let label = format!("{:<label_width$}", account.label);
    let label = if chosen {
        Span::styled(label, super::bold().reversed())
    } else {
        Span::raw(label)
    };
    let busiest = account
        .usage
        .iter()
        .max_by(|a, b| a.used_percent.total_cmp(&b.used_percent))
        .map_or_else(String::new, |usage| {
            format!(
                " · {} {:.0}%",
                account_screen::window_label(&usage.window),
                usage.used_percent
            )
        });
    Line::from(vec![
        Span::styled(mark, Style::new().fg(Color::Cyan)),
        label,
        Span::styled(
            format!("  {}{busiest}", account.provider.as_str()),
            super::dim(),
        ),
    ])
}

/// What switching `session` to `account` does, for the dialog.
fn what(session: &Session, account: &Account) -> String {
    match switch::kind(session, account) {
        Kind::Current => "The session is on this account: a model changes only the model.".into(),
        Kind::Account => format!(
            "Moves to {} between turns; the conversation carries on.",
            account.label
        ),
        Kind::Provider => format!(
            "Moves to {} between turns, replaying the transcript there.",
            account.provider.as_str()
        ),
    }
}
