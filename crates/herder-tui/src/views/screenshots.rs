//! Screenshots: named scenes drawn from fake client state through the real views and widgets,
//! written as PNGs by Charm's `freeze`, at 45, 100 and 160 columns in the dark and light
//! `herder` themes.
//!
//! `scripts/screenshots.sh <todo>` runs this into `docs/screenshots/<todo>/`; it is ignored
//! otherwise. Add a scene to [`SCENES`] for each screen a change touches.

use std::fmt::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};

use crate::app::{App, Msg};
use crate::fake;
use crate::ui::Ui;
use crate::ui::gallery::Gallery;
use crate::ui::glyphs::Glyphs;
use crate::ui::theme::{Mode, Theme};

/// Screen sizes: a phone, a laptop terminal, a wide one.
const SIZES: [(u16, u16); 3] = [(45, 40), (100, 30), (160, 40)];

/// A scene: its name and what draws it.
type Scene = (&'static str, fn(&Theme, Mode, u16, u16) -> Buffer);

/// Every scene.
const SCENES: [Scene; 4] = [
    ("components", |theme, mode, width, height| {
        gallery(theme, mode, width, height, false)
    }),
    ("dialog", |theme, mode, width, height| {
        gallery(theme, mode, width, height, true)
    }),
    ("session", |theme, _, width, height| {
        app_buffer(super::tests::mid_turn(), theme, width, height)
    }),
    ("help", |theme, _, width, height| {
        let mut app = fake::tree();
        app.update(Msg::Key(KeyEvent::new(
            KeyCode::Char('?'),
            KeyModifiers::NONE,
        )));
        app_buffer(app, theme, width, height)
    }),
];

fn gallery(theme: &Theme, mode: Mode, width: u16, height: u16, dialog: bool) -> Buffer {
    // The last column stays blank, as the TUI leaves it.
    let area = Rect::new(0, 0, width - 1, height);
    let mut buffer = Buffer::empty(Rect::new(0, 0, width, height));
    let glyphs = Glyphs::for_width(None, area.width);
    let ui = Ui::new(theme, glyphs);
    buffer.set_style(buffer.area, ui.base());
    let mut gallery = Gallery {
        dialog,
        ..Gallery::default()
    };
    let label = format!(
        "herder {}{}{}",
        mode.name(),
        glyphs.set().separator,
        glyphs.name()
    );
    gallery.draw(ui, area, &mut buffer, &label);
    buffer
}

fn app_buffer(mut app: App, theme: &Theme, width: u16, height: u16) -> Buffer {
    app.theme = theme.clone();
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| super::draw(frame, &mut app)).unwrap();
    terminal.backend().buffer().clone()
}

/// `buffer` as text with 24-bit SGR colours; the terminal's own colours are `theme`'s.
fn ansi(buffer: &Buffer, theme: &Theme) -> String {
    let resolve = |color: Color, default: Color| {
        if color == Color::Reset {
            default
        } else {
            color
        }
    };
    let mut out = String::new();
    let area = buffer.area;
    for y in area.top()..area.bottom() {
        let mut skip = 0;
        for x in area.left()..area.right() {
            if skip > 0 {
                skip -= 1;
                continue;
            }
            let cell = &buffer[(x, y)];
            let mut fg = resolve(cell.fg, theme.text);
            let mut bg = resolve(cell.bg, theme.background);
            if cell.modifier.contains(Modifier::REVERSED) {
                std::mem::swap(&mut fg, &mut bg);
            }
            let _ = write!(out, "\x1b[0;{};{}", sgr(fg, false), sgr(bg, true));
            for (modifier, code) in [
                (Modifier::BOLD, 1),
                (Modifier::DIM, 2),
                (Modifier::ITALIC, 3),
                (Modifier::UNDERLINED, 4),
                (Modifier::CROSSED_OUT, 9),
            ] {
                if cell.modifier.contains(modifier) {
                    let _ = write!(out, ";{code}");
                }
            }
            out.push('m');
            out.push_str(cell.symbol());
            skip = crate::ui::width(cell.symbol()).saturating_sub(1);
        }
        out.push_str("\x1b[0m\n");
    }
    out
}

/// The SGR parameters for `color` as a foreground, or a background.
fn sgr(color: Color, background: bool) -> String {
    let base = if background { 40 } else { 30 };
    let named = |index: u8| {
        if index < 8 {
            format!("{}", base + index)
        } else {
            format!("{}", base + 60 + index - 8)
        }
    };
    match color {
        Color::Rgb(r, g, b) => format!("{};2;{r};{g};{b}", base + 8),
        Color::Indexed(index) => format!("{};5;{index}", base + 8),
        Color::Black => named(0),
        Color::Red => named(1),
        Color::Green => named(2),
        Color::Yellow => named(3),
        Color::Blue => named(4),
        Color::Magenta => named(5),
        Color::Cyan => named(6),
        Color::Gray => named(7),
        Color::DarkGray => named(8),
        Color::LightRed => named(9),
        Color::LightGreen => named(10),
        Color::LightYellow => named(11),
        Color::LightBlue => named(12),
        Color::LightMagenta => named(13),
        Color::LightCyan => named(14),
        Color::White => named(15),
        Color::Reset => format!("{}", base + 9),
    }
}

fn hex(color: Color) -> String {
    match color {
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        _ => "#000000".to_owned(),
    }
}

/// Writes `<scene>-<width>-<mode>.png` for every scene, size and mode into
/// `$HERDER_SCREENSHOTS`.
#[test]
#[ignore = "run by scripts/screenshots.sh: needs freeze"]
fn screenshots() {
    let out = std::env::var_os("HERDER_SCREENSHOTS").expect("set HERDER_SCREENSHOTS");
    let out = Path::new(&out);
    std::fs::create_dir_all(out).unwrap();
    let only = std::env::var("HERDER_SCENES")
        .ok()
        .filter(|only| !only.is_empty());
    for (name, draw) in SCENES {
        if only
            .as_deref()
            .is_some_and(|only| !only.split(',').any(|scene| scene == name))
        {
            continue;
        }
        for mode in [Mode::Dark, Mode::Light] {
            let theme = Theme::herder(mode);
            for (width, height) in SIZES {
                let buffer = draw(&theme, mode, width, height);
                let stem = format!("{name}-{width}-{}", mode.name());
                let source = out.join(format!("{stem}.ansi"));
                std::fs::write(&source, ansi(&buffer, &theme)).unwrap();
                let status = Command::new("freeze")
                    .arg(&source)
                    .args(["--language", "ansi", "--window=false"])
                    .args(["--background", &hex(theme.background)])
                    .args(["--padding", "16", "--margin", "0", "--border.radius", "0"])
                    .args(["--font.size", "14", "--line-height", "1.15"])
                    .arg("--output")
                    .arg(out.join(format!("{stem}.png")))
                    // Given no terminal, freeze reads stdin too.
                    .stdin(Stdio::null())
                    .status()
                    .expect("run freeze: install it from github.com/charmbracelet/freeze");
                assert!(status.success(), "freeze failed on {stem}");
                std::fs::remove_file(&source).unwrap();
            }
        }
    }
}
