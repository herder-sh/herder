//! The terminal picker, over everything else: a new shell, or one of the session's open ones.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Padding};

use crate::app::App;
use crate::terminal::{self, Target};

pub(super) fn draw(frame: &mut Frame, area: Rect, app: &App) {
    let Some(picker) = &app.terminals else {
        return;
    };
    let rows = terminal::rows(&app.machines, &picker.session);
    let title = app
        .sessions
        .get(&picker.session)
        .map_or_else(String::new, |session| session.title());
    let items: Vec<ListItem> = rows
        .iter()
        .map(|row| match row {
            Target::New(_) => ListItem::new(Line::styled("+ new terminal", super::bold())),
            Target::Existing(terminal_id) => ListItem::new(format!("  terminal {terminal_id}")),
        })
        .collect();
    let height = u16::try_from(rows.len() + 4).unwrap_or(u16::MAX);
    let popup = super::centered(area, 52, height);
    let list = List::new(items)
        .block(
            Block::bordered()
                .title(format!(" terminals · {title} "))
                .title_bottom(Line::styled(" Enter attach  Esc close ", super::dim()).centered())
                .padding(Padding::uniform(1)),
        )
        .highlight_style(Style::new().add_modifier(Modifier::REVERSED));
    let mut state = ListState::default().with_selected(Some(picker.selected.min(rows.len() - 1)));
    frame.render_widget(Clear, popup);
    frame.render_stateful_widget(list, popup, &mut state);
}

#[cfg(test)]
mod tests {
    use herder_protocol::Role;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use crate::app::{App, Msg};
    use crate::terminal::app_tests::with_terminals;

    fn render(app: &mut App, width: u16, height: u16) -> Terminal<TestBackend> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| super::super::draw(frame, app))
            .unwrap();
        terminal
    }

    fn press(app: &mut App, code: KeyCode) {
        app.update(Msg::Key(KeyEvent::new(code, KeyModifiers::NONE)));
    }

    #[test]
    fn the_picker_offers_a_new_terminal_and_the_open_ones() {
        let mut app = with_terminals();
        press(&mut app, KeyCode::Char('t'));
        press(&mut app, KeyCode::Char('j'));
        insta::assert_snapshot!(render(&mut app, 80, 14).backend());
    }

    #[test]
    fn a_member_sees_terminals_are_owner_only() {
        let mut app = with_terminals();
        let mut machines = app.machines.clone();
        machines[0].role = Some(Role::Member);
        machines[0].terminals.clear();
        app.update(Msg::Machines(machines));
        press(&mut app, KeyCode::Char('t'));
        insta::assert_snapshot!(render(&mut app, 80, 8).backend());
    }

    #[test]
    fn an_exited_terminal_is_reported() {
        let mut app = with_terminals();
        app.update(Msg::TerminalEnded(crate::terminal::Ended::Exited(Some(1))));
        insta::assert_snapshot!(render(&mut app, 80, 8).backend());
    }
}
