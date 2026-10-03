//! Every shared widget on one screen, to drive by hand: `cargo run -p herder-tui --example
//! gallery`. Tab moves between the list and the prompt; ctrl+t cycles the theme (herder dark,
//! herder light, ansi), ctrl+g the glyph set, ctrl+o opens the go-to dialog; esc quits.

use std::io;

use herder_tui::ui::Ui;
use herder_tui::ui::gallery::{Focus, Gallery};
use herder_tui::ui::glyphs::Glyphs;
use herder_tui::ui::theme::{Mode, Theme};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::Rect;

fn main() -> io::Result<()> {
    let themes = [
        ("herder dark", Theme::herder(Mode::Dark)),
        ("herder light", Theme::herder(Mode::Light)),
        ("ansi", Theme::ansi()),
    ];
    let mut theme = 0;
    let mut glyphs: Option<Glyphs> = None;
    let mut gallery = Gallery::default();
    let mut terminal = ratatui::init();
    let result = loop {
        let drawn = terminal.draw(|frame| {
            let screen = frame.area();
            // The last column stays blank, as the TUI leaves it.
            let area = Rect {
                width: screen.width.saturating_sub(1),
                ..screen
            };
            let set = Glyphs::for_width(glyphs, area.width);
            let (name, theme) = &themes[theme];
            let ui = Ui::new(theme, set);
            let label = format!("{name}{}{}", set.set().separator, set.name());
            gallery.draw(ui, area, frame.buffer_mut(), &label);
        });
        if let Err(err) = drawn {
            break Err(err);
        }
        let key = match event::read() {
            Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => key,
            Ok(_) => continue,
            Err(err) => break Err(err),
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc if gallery.dialog => gallery.dialog = false,
            KeyCode::Esc => break Ok(()),
            KeyCode::Char('c') if ctrl => break Ok(()),
            KeyCode::Char('t') if ctrl => theme = (theme + 1) % themes.len(),
            KeyCode::Char('g') if ctrl => {
                glyphs = match glyphs {
                    None | Some(Glyphs::Unicode) => Some(Glyphs::Ascii),
                    Some(Glyphs::Ascii) => Some(Glyphs::Unicode),
                }
            }
            KeyCode::Char('o') if ctrl => gallery.dialog = !gallery.dialog,
            KeyCode::Tab => {
                gallery.focus = match gallery.focus {
                    Focus::List => Focus::Prompt,
                    Focus::Prompt => Focus::List,
                }
            }
            KeyCode::Char('j') | KeyCode::Down if gallery.focus == Focus::List => gallery.step(1),
            KeyCode::Char('k') | KeyCode::Up if gallery.focus == Focus::List => gallery.step(-1),
            _ if gallery.dialog => {
                gallery.search.input(key);
            }
            _ if gallery.focus == Focus::Prompt => {
                gallery.prompt.input(key);
            }
            _ => {}
        }
    };
    ratatui::restore();
    result
}
