//! The new-session dialog, over everything else.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Padding};

use crate::app::App;
use crate::compose::Field;
use crate::session::mode_name;

/// Width of the field labels.
const LABEL: u16 = 9;

pub(super) fn draw(frame: &mut Frame, area: Rect, app: &mut App) {
    let Some(dialog) = &mut app.compose.dialog else {
        return;
    };
    let machine = app.machines.iter().find(|m| m.host_id == dialog.host_id);
    let machine_name = machine.map_or_else(|| dialog.host_id.to_string(), |m| m.name.clone());
    let account = machine
        .and_then(|m| m.accounts.get(dialog.account))
        .map_or_else(
            || "no accounts".to_owned(),
            |a| format!("{} ({})", a.label, a.account_id),
        );
    let popup = super::centered(area, 64, 13);
    let footer = match (&dialog.error, dialog.sending) {
        (Some(error), _) => Line::styled(format!(" {error} "), Style::new().fg(Color::Red)),
        (None, true) => Line::styled(" creating… ", super::dim()),
        (None, false) => Line::styled(
            " Enter create · Tab next · ←/→ change · Esc cancel ",
            super::dim(),
        ),
    };
    let block = Block::bordered()
        .title(" new session ")
        .title_bottom(footer.centered())
        .border_style(Style::new().fg(Color::Cyan))
        .padding(Padding::uniform(1));
    let inner = block.inner(popup);
    frame.render_widget(Clear, popup);
    frame.render_widget(block, popup);
    let rows: [Rect; 9] = Layout::vertical([Constraint::Length(1); 9]).areas(inner);
    let fields = [
        (Field::Machine, "machine", rows[0]),
        (Field::Repo, "repo", rows[2]),
        (Field::Account, "account", rows[4]),
        (Field::Model, "model", rows[6]),
        (Field::Mode, "mode", rows[8]),
    ];
    for (field, label, row) in fields {
        let focused = dialog.field == field;
        let [label_area, value] =
            Layout::horizontal([Constraint::Length(LABEL), Constraint::Fill(1)]).areas(row);
        let label_style = if focused {
            Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD)
        } else {
            super::dim()
        };
        frame.render_widget(Line::styled(label, label_style), label_area);
        let choice = |text: String| {
            let arrows = if focused { super::bold() } else { super::dim() };
            Line::from(vec![
                Span::styled("‹ ", arrows),
                Span::raw(text),
                Span::styled(" ›", arrows),
            ])
        };
        let cursor = if focused {
            Style::new().add_modifier(Modifier::REVERSED)
        } else {
            Style::new()
        };
        match field {
            Field::Machine => frame.render_widget(choice(machine_name.clone()), value),
            Field::Account => frame.render_widget(choice(account.clone()), value),
            Field::Mode => frame.render_widget(choice(mode_name(dialog.mode).to_owned()), value),
            Field::Repo => {
                dialog.repo.set_cursor_style(cursor);
                frame.render_widget(&dialog.repo, value);
            }
            Field::Model => {
                dialog.model.set_cursor_style(cursor);
                frame.render_widget(&dialog.model, value);
            }
        }
    }
}
