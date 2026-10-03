//! Snapshots of widgets in every theme and glyph set: the text, then each run of styled cells,
//! so a colour change shows in the diff.

use std::fmt::Write;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::Widget;

use super::Ui;
use super::glyphs::Glyphs;
use super::theme::{Mode, Theme};

/// A theme and glyph set to snapshot a widget in.
pub struct Variant {
    pub name: &'static str,
    pub theme: Theme,
    pub glyphs: Glyphs,
}

impl Variant {
    pub fn ui(&self) -> Ui<'_> {
        Ui::new(&self.theme, self.glyphs)
    }
}

/// `herder` dark and light in both glyph sets, and `ansi`.
pub fn variants() -> Vec<Variant> {
    let herder = |name, mode, glyphs| Variant {
        name,
        theme: Theme::herder(mode),
        glyphs,
    };
    vec![
        herder("dark-unicode", Mode::Dark, Glyphs::Unicode),
        herder("dark-ascii", Mode::Dark, Glyphs::Ascii),
        herder("light-unicode", Mode::Light, Glyphs::Unicode),
        herder("light-ascii", Mode::Light, Glyphs::Ascii),
        Variant {
            name: "ansi-unicode",
            theme: Theme::ansi(),
            glyphs: Glyphs::Unicode,
        },
    ]
}

/// Snapshots, as `<name>-<variant>`, what `draw` draws in each variant.
pub fn each(name: &str, draw: impl Fn(&Variant) -> Buffer) {
    for variant in variants() {
        let buffer = draw(&variant);
        insta::assert_snapshot!(format!("{name}-{}", variant.name), dump(&buffer));
    }
}

/// A `width` by `height` buffer on the screen's background, drawn by `draw`.
pub fn render(
    variant: &Variant,
    width: u16,
    height: u16,
    draw: impl FnOnce(Ui, Rect, &mut Buffer),
) -> Buffer {
    let area = Rect::new(0, 0, width, height);
    let mut buffer = Buffer::empty(area);
    let ui = variant.ui();
    buffer.set_style(area, ui.base());
    draw(ui, area, &mut buffer);
    buffer
}

/// `lines`, one a row, `width` wide.
pub fn lines(variant: &Variant, width: u16, lines: Vec<Line<'static>>) -> Buffer {
    let height = u16::try_from(lines.len()).unwrap();
    render(variant, width, height, |_, area, buf| {
        for (line, y) in lines.into_iter().zip(area.y..) {
            line.render(
                Rect {
                    y,
                    height: 1,
                    ..area
                },
                buf,
            );
        }
    })
}

/// The buffer's text, then its style runs: `y x0..x1 fg bg modifiers`.
pub fn dump(buffer: &Buffer) -> String {
    let area = buffer.area;
    let mut out = String::new();
    for y in area.top()..area.bottom() {
        let mut row = String::new();
        let mut skip = 0;
        for x in area.left()..area.right() {
            if skip > 0 {
                skip -= 1;
                continue;
            }
            let symbol = buffer[(x, y)].symbol();
            skip = super::width(symbol).saturating_sub(1);
            row.push_str(symbol);
        }
        let _ = writeln!(out, "|{}|", row);
    }
    out.push('\n');
    for y in area.top()..area.bottom() {
        let mut x = area.left();
        while x < area.right() {
            let style = buffer[(x, y)].style();
            let start = x;
            while x < area.right() && buffer[(x, y)].style() == style {
                x += 1;
            }
            let _ = writeln!(out, "{y:>2} {start:>3}..{x:<3} {}", describe(style));
        }
    }
    out
}

fn describe(style: Style) -> String {
    let color = |color: Option<Color>| match color {
        Some(Color::Rgb(r, g, b)) => format!("#{r:02x}{g:02x}{b:02x}"),
        Some(color) => format!("{color:?}"),
        None => "-".to_owned(),
    };
    let mut text = format!("fg={} bg={}", color(style.fg), color(style.bg));
    if style.add_modifier != Modifier::empty() {
        let _ = write!(text, " {:?}", style.add_modifier);
    }
    text
}
