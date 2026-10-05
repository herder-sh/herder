//! The terminal picker, over everything else: a new shell, or one of the session's open ones.
//!
//! ```text
//! ┌─ terminals · app · api ───────────────── esc ─┐
//! │                                               │
//! │▶ + new terminal                               │
//! │                                               │
//! │  open shells                                1 │
//! │  $ terminal t1                                │
//! └───────────────────────────────────────────────┘
//! ```

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};

use crate::app::App;
use crate::mouse::{Click, Hits, List as Rows};
use crate::terminal::{self, Target};
use crate::ui::dialog::{Dialog, Size};
use crate::ui::hints::Hint;
use crate::ui::list::{ListView, Row};

pub(super) fn draw(frame: &mut Frame, area: Rect, app: &App, hits: &mut Hits) {
    let Some(picker) = &app.terminals else {
        return;
    };
    let ui = app.ui();
    let targets = terminal::rows(&app.machines, &picker.session);
    let title = app
        .sessions
        .get(&picker.session)
        .map_or_else(String::new, |session| session.short_title());
    let mut rows = Vec::new();
    // The target of each row; the gap and the heading have none.
    let mut of_row = Vec::new();
    let open = targets
        .iter()
        .filter(|target| !matches!(target, Target::New(_)))
        .count();
    for (at, target) in targets.iter().enumerate() {
        let item = match target {
            Target::New(_) => Row::item(Line::from(vec![
                Span::styled("+", ui.accent()),
                Span::styled(" new terminal", ui.text()),
            ])),
            Target::Existing(terminal_id) => Row::item(Line::from(vec![
                Span::styled(ui.glyphs.tools[0], ui.muted()),
                Span::styled(format!(" terminal {terminal_id}"), ui.text()),
            ])),
            // Never a picker row: a login is started from the machines panel.
            Target::Login(account) => Row::item(Line::styled(
                format!("login {}", account.account_id),
                ui.text(),
            )),
            Target::LogInAgain(account_id) => {
                Row::item(Line::styled(format!("login {account_id}"), ui.text()))
            }
        };
        if at == 1 {
            rows.push(Row::Gap);
            rows.push(Row::header("open shells").right(open.to_string()));
            of_row.extend([None, None]);
        }
        rows.push(item);
        of_row.push(Some(at));
    }
    let selected = picker.selected.min(targets.len().saturating_sub(1));
    let hints = [Hint::new("enter", "attach"), Hint::new("esc", "close")];
    let height = u16::try_from(rows.iter().map(Row::height).sum::<usize>()).unwrap_or(u16::MAX);
    let title = Line::from(ui.joined([Span::raw("terminals"), Span::raw(title)]));
    let areas =
        Dialog::new(ui, title, Size::Medium)
            .hints(&hints)
            .render(area, height, frame.buffer_mut());
    super::palette::dialog_taps(hits, area, &areas);
    // As a picker's: the pointer sits in the padding, the cursor's row a column past the
    // text either side.
    let body = areas.body;
    let list = Rect::new(
        body.x.saturating_sub(1),
        body.y,
        body.width + 2,
        body.height,
    )
    .intersection(areas.outer);
    let mut offset = 0;
    let placed = ListView::new(ui, rows)
        .select(of_row.iter().position(|at| *at == Some(selected)))
        .focused(true)
        .render(list, frame.buffer_mut(), &mut offset);
    for (row, rect) in placed {
        if let Some(Some(at)) = of_row.get(row) {
            hits.click(rect, Click::Row(Rows::Picker, *at));
        }
    }
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
