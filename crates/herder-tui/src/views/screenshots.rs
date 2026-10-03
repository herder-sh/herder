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
const SCENES: [Scene; 43] = [
    ("components", |theme, mode, width, height| {
        gallery(theme, mode, width, height, false)
    }),
    ("dialog", |theme, mode, width, height| {
        gallery(theme, mode, width, height, true)
    }),
    ("session", |theme, _, width, height| {
        app_buffer(super::tests::mid_turn(), theme, width, height)
    }),
    ("shell", |theme, _, width, height| {
        app_buffer(super::tests::herd(), theme, width, height)
    }),
    ("approval", |theme, _, width, height| {
        let mut app = super::tests::herd();
        let asked = vec![fake::approval("a1", "rm -rf target/")];
        fake::feed(
            &mut app,
            "h1",
            "s2",
            fake::update("s2", 20, asked, Vec::new()),
        );
        // Out of the prompt and back: the request takes it.
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Esc);
        app_buffer(app, theme, width, height)
    }),
    ("switcher", |theme, _, width, height| {
        let mut app = super::tests::herd();
        app.act(crate::action::Action::GoTo);
        press(&mut app, KeyCode::Char('j'));
        app_buffer(app, theme, width, height)
    }),
    ("leader", |theme, _, width, height| {
        let mut app = super::tests::herd();
        app.update(Msg::Key(KeyEvent::new(
            KeyCode::Char('x'),
            KeyModifiers::CONTROL,
        )));
        app_buffer(app, theme, width, height)
    }),
    ("collapsed", |theme, _, width, height| {
        let mut app = super::tests::herd();
        app.layout.collapsed = true;
        press(&mut app, KeyCode::Esc);
        app_buffer(app, theme, width, height)
    }),
    ("chat", |theme, _, width, height| {
        let mut app = fake::chat();
        fake::type_text(
            &mut app,
            "write the docs page too, and link it from @README.md",
        );
        app_buffer(app, theme, width, height)
    }),
    ("approve", |theme, _, width, height| {
        app_buffer(fake::chat_approval(), theme, width, height)
    }),
    ("question", |theme, _, width, height| {
        app_buffer(fake::chat_question(), theme, width, height)
    }),
    ("commands", |theme, _, width, height| {
        let mut app = fake::chat();
        fake::type_text(&mut app, "/mo");
        app_buffer(app, theme, width, height)
    }),
    ("navigate", |theme, _, width, height| {
        let mut app = fake::chat();
        app.focus = crate::app::Focus::Transcript;
        for id in ["r1", "c3", "c5"] {
            app.chat.expanded.insert(herder_protocol::ItemId::new(id));
        }
        app.chat.cursor = Some(herder_protocol::ItemId::new("c3"));
        app.chat.reveal = true;
        app_buffer(app, theme, width, height)
    }),
    ("attach-prompt", |theme, _, width, height| {
        let mut app = fake::chat();
        for bytes in [348_160, 1_258_291] {
            attached(&mut app, Ok(bytes));
        }
        app.compose.loading = 1;
        fake::type_text(&mut app, "the sidebar overlaps the list here, fix it");
        app_buffer(app, theme, width, height)
    }),
    ("attach-popup", |theme, _, width, height| {
        let mut app = fake::chat();
        fake::type_text(&mut app, "match @shots/");
        let entry = |name: &str, is_dir| crate::attach::Entry {
            name: name.into(),
            is_dir,
        };
        app.update(Msg::Listed {
            dir: "shots/".into(),
            entries: vec![
                entry("mockups", true),
                entry("login-dark.png", false),
                entry("login-light.png", false),
                entry("sidebar.webp", false),
            ],
        });
        app_buffer(app, theme, width, height)
    }),
    ("attach-error", |theme, _, width, height| {
        let mut app = fake::chat();
        attached(&mut app, Ok(348_160));
        attached(&mut app, Err("raw.png: 7.4 MB, over the 5 MB limit"));
        fake::type_text(&mut app, "the sidebar overlaps the list here");
        app_buffer(app, theme, width, height)
    }),
    ("attach-transcript", |theme, _, width, height| {
        let mut app = fake::chat();
        let key = fake::key("h1", "s2");
        let session = app.sessions.get_mut(&key).unwrap();
        let mut first = None;
        for entry in &mut session.entries {
            if let crate::session::Entry::Item(item) = entry
                && let herder_protocol::ItemBody::UserMessage { attachments, .. } = &mut item.body
            {
                *attachments = [
                    ("01J9A", "image/png", 348_160),
                    ("01J9B", "image/jpeg", 1_258_291),
                ]
                .map(|(id, media_type, size)| herder_protocol::Attachment {
                    attachment_id: herder_protocol::AttachmentId::new(id),
                    media_type: media_type.into(),
                    size,
                })
                .to_vec();
                first = Some(item.id.clone());
                break;
            }
        }
        app.focus = crate::app::Focus::Transcript;
        app.chat.cursor = first;
        app.chat.reveal = true;
        app_buffer(app, theme, width, height)
    }),
    ("help", |theme, _, width, height| {
        let mut app = fake::tree();
        press(&mut app, KeyCode::Char('?'));
        app_buffer(app, theme, width, height)
    }),
    ("palette", |theme, _, width, height| {
        let mut app = super::tests::mid_turn();
        app.update(Msg::Key(KeyEvent::new(
            KeyCode::Char('p'),
            KeyModifiers::CONTROL,
        )));
        app_buffer(app, theme, width, height)
    }),
    ("palette-search", |theme, _, width, height| {
        let mut app = super::tests::mid_turn();
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char(':'));
        fake::type_text(&mut app, "mode");
        app_buffer(app, theme, width, height)
    }),
    ("new-session-project", |theme, _, width, height| {
        let mut app = fake::projects();
        press(&mut app, KeyCode::Char('v'));
        press(&mut app, KeyCode::Char('n'));
        app_buffer(app, theme, width, height)
    }),
    ("new-session-machine", |theme, _, width, height| {
        let mut app = fake::projects();
        press(&mut app, KeyCode::Char('n'));
        app_buffer(app, theme, width, height)
    }),
    ("new-session-form", |theme, _, width, height| {
        let mut app = fake::projects();
        press(&mut app, KeyCode::Char('n'));
        press(&mut app, KeyCode::Enter);
        app_buffer(app, theme, width, height)
    }),
    ("switch", |theme, _, width, height| {
        let mut app = super::tests::mid_turn();
        super::tests::add_accounts(&mut app);
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('s'));
        app_buffer(app, theme, width, height)
    }),
    ("add-machine", |theme, _, width, height| {
        let mut app = fake::tree();
        press(&mut app, KeyCode::Char('a'));
        press(&mut app, KeyCode::Tab);
        fake::type_text(&mut app, "10.0.0.4");
        app_buffer(app, theme, width, height)
    }),
    ("add-machine-confirm", |theme, _, width, height| {
        let mut app = fake::tree();
        app.update(Msg::Paste(super::tests::LINK.to_owned()));
        app_buffer(app, theme, width, height)
    }),
    ("inbox", |theme, _, width, height| {
        app_buffer(inbox(), theme, width, height)
    }),
    ("inbox-answer", |theme, _, width, height| {
        let mut app = inbox();
        press(&mut app, KeyCode::Enter);
        fake::type_text(&mut app, "8080, behind the proxy");
        app_buffer(app, theme, width, height)
    }),
    ("prs", |theme, _, width, height| {
        let mut app = fake::projects();
        press(&mut app, KeyCode::Char('P'));
        app_buffer(app, theme, width, height)
    }),
    ("prs-tab", |theme, _, width, height| {
        let mut app = fake::with_prs();
        press(&mut app, KeyCode::Char('p'));
        press(&mut app, KeyCode::Char('j'));
        app_buffer(app, theme, width, height)
    }),
    ("accounts", |theme, _, width, height| {
        let mut app = fake::tree();
        super::tests::add_accounts(&mut app);
        press(&mut app, KeyCode::Char('A'));
        app_buffer(app, theme, width, height)
    }),
    ("fleet", |theme, _, width, height| {
        let mut app = fake::tree();
        super::tests::add_accounts(&mut app);
        fake::with_resources(&mut app, fake::host_resources(4), false);
        press(&mut app, KeyCode::Char('m'));
        app_buffer(app, theme, width, height)
    }),
    ("limit-reset", |theme, _, width, height| {
        app_buffer(super::tests::limit_reset(), theme, width, height)
    }),
    ("backup-fleet", |theme, _, width, height| {
        let mut app = fake::backups();
        press(&mut app, KeyCode::Char('j'));
        app_buffer(app, theme, width, height)
    }),
    ("backup-pick", |theme, _, width, height| {
        app_buffer(backup(0, &[]), theme, width, height)
    }),
    ("backup-linked", |theme, _, width, height| {
        app_buffer(backup(1, &[]), theme, width, height)
    }),
    ("backup-stop", |theme, _, width, height| {
        app_buffer(backup(1, &[KeyCode::Enter]), theme, width, height)
    }),
    ("backup-done", |theme, _, width, height| {
        let mut app = backup(0, &[]);
        let mut effects = app.update(Msg::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
        let code = Ok(herder_protocol::CommandResult::HostPairing {
            code: "ABCDE-FGHJK".into(),
            expires_at: herder_protocol::Timestamp::UNIX_EPOCH,
        });
        for result in [code, Ok(herder_protocol::CommandResult::Applied)] {
            let Some(crate::app::Effect::Send { origin, .. }) = effects.pop() else {
                break;
            };
            effects = app.update(Msg::Sent { origin, result });
        }
        app_buffer(app, theme, width, height)
    }),
    ("resources", |theme, _, width, height| {
        let mut app = super::tests::mid_turn();
        fake::with_resources(&mut app, fake::host_resources(2), true);
        app_buffer(app, theme, width, height)
    }),
    ("fork", |theme, _, width, height| {
        let mut app = fake::vault();
        app.choose_row(crate::app::Row::Session {
            key: fake::key("v", "s2"),
            depth: 0,
        });
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Char('F'));
        app_buffer(app, theme, width, height)
    }),
    ("terminals", |theme, _, width, height| {
        let mut app = crate::terminal::app_tests::with_terminals();
        press(&mut app, KeyCode::Char('t'));
        press(&mut app, KeyCode::Char('j'));
        app_buffer(app, theme, width, height)
    }),
    ("live", |theme, _, width, height| {
        app_buffer(fake::live(), theme, width, height)
    }),
    ("live-expanded", |theme, _, width, height| {
        let mut app = fake::live();
        press(&mut app, KeyCode::Esc);
        for id in ["c3", "c4"] {
            app.chat.expanded.insert(herder_protocol::ItemId::new(id));
        }
        press(&mut app, KeyCode::Char('H'));
        app_buffer(app, theme, width, height)
    }),
];

/// [`fake::escalated`] in the inbox, two minutes after the escalation.
fn inbox() -> App {
    let mut app = fake::escalated();
    app.clock = Some(herder_protocol::Timestamp::from_second(320).unwrap());
    press(&mut app, KeyCode::Char('i'));
    app
}

/// An image of `bytes` loaded for the prompt, or why it could not be.
fn attached(app: &mut App, bytes: Result<usize, &str>) {
    let result = bytes
        .map(|bytes| herder_protocol::Image {
            media_type: "image/png".into(),
            data: herder_protocol::Bytes(vec![0; bytes]),
        })
        .map_err(str::to_owned);
    app.compose.loading += 1;
    app.update(Msg::Attached { word: None, result });
}

/// Presses `code`.
/// [`fake::backups`] with the backup dialog open on machine `at`, then `keys` pressed.
fn backup(at: usize, keys: &[KeyCode]) -> App {
    let mut app = fake::backups();
    for _ in 0..at {
        press(&mut app, KeyCode::Char('j'));
    }
    let links = app.machine_panel.as_ref().map(|p| p.links.clone());
    press(&mut app, KeyCode::Char('b'));
    // As the machines answer again.
    if let (Some(panel), Some(links)) = (&mut app.machine_panel, links) {
        panel.links = links;
    }
    for key in keys {
        press(&mut app, *key);
    }
    app
}

fn press(app: &mut App, code: KeyCode) {
    app.update(Msg::Key(KeyEvent::new(code, KeyModifiers::NONE)));
}

/// [`super::tests::mid_turn`], waiting on an approval, or with `question`, a question.
fn asking(question: bool) -> App {
    let mut app = super::tests::mid_turn();
    let since = herder_protocol::Timestamp::now().as_second() - 12;
    let body = if question {
        fake::question(
            "q1",
            "Which heading level for the API page?",
            &["h2 under Reference", "h1, its own page"],
        )
    } else {
        fake::approval("a1", "Bash: rm -rf target/")
    };
    fake::feed(
        &mut app,
        "h1",
        "s2",
        fake::at(fake::update("s2", 20, vec![body], Vec::new()), since),
    );
    app
}

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

/// Writes `frame-NN.png` into `$HERDER_FRAMES`: the dialogs driven key by key at 100
/// columns in the dark theme, for `scripts/screenshots.sh`'s GIF.
#[test]
#[ignore = "run by hand for a PR's GIF: needs freeze"]
fn frames() {
    let out = std::env::var_os("HERDER_FRAMES").expect("set HERDER_FRAMES");
    let out = Path::new(&out);
    std::fs::create_dir_all(out).unwrap();
    let theme = Theme::herder(Mode::Dark);
    let mut app = asking(false);
    super::tests::add_accounts(&mut app);
    let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
    let mut keys = vec![
        None,
        Some(key(KeyCode::Right)),
        Some(key(KeyCode::Left)),
        Some(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL)),
        Some(key(KeyCode::Char('s'))),
        Some(key(KeyCode::Char('w'))),
        Some(key(KeyCode::Enter)),
        Some(key(KeyCode::Down)),
        Some(key(KeyCode::Down)),
        Some(key(KeyCode::Tab)),
    ];
    keys.extend("gpt-5".chars().map(|c| Some(key(KeyCode::Char(c)))));
    keys.extend(
        [
            KeyCode::Esc,
            KeyCode::Esc,
            KeyCode::Esc,
            KeyCode::Char('n'),
            KeyCode::Enter,
            KeyCode::Enter,
            KeyCode::Right,
            KeyCode::Tab,
            KeyCode::Tab,
            KeyCode::Right,
        ]
        .map(|code| Some(key(code))),
    );
    for (at, pressed) in keys.into_iter().enumerate() {
        if let Some(pressed) = pressed {
            app.update(Msg::Key(pressed));
        }
        let buffer = app_buffer_ref(&mut app, &theme, 100, 30);
        let source = out.join(format!("frame-{at:02}.ansi"));
        std::fs::write(&source, ansi(&buffer, &theme)).unwrap();
        let status = Command::new("freeze")
            .arg(&source)
            .args(["--language", "ansi", "--window=false"])
            .args(["--background", &hex(theme.background)])
            .args(["--padding", "16", "--margin", "0", "--border.radius", "0"])
            .args(["--font.size", "14", "--line-height", "1.15"])
            .arg("--output")
            .arg(out.join(format!("frame-{at:02}.png")))
            .stdin(Stdio::null())
            .status()
            .expect("run freeze");
        assert!(status.success());
        std::fs::remove_file(&source).unwrap();
    }
}

/// [`app_buffer`] without giving `app` up.
fn app_buffer_ref(app: &mut App, theme: &Theme, width: u16, height: u16) -> Buffer {
    app.theme = theme.clone();
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| super::draw(frame, app)).unwrap();
    terminal.backend().buffer().clone()
}

/// ANSI frames replayed by vhs for the limit-reset PR's short interaction recording.
#[test]
#[ignore = "run for the limit-reset PR with HERDER_FRAMES"]
fn limit_reset_frames() {
    let out = std::env::var_os("HERDER_FRAMES").expect("set HERDER_FRAMES");
    let out = Path::new(&out);
    std::fs::create_dir_all(out).unwrap();
    let theme = Theme::herder(Mode::Dark);
    let mut app = super::tests::limit_reset();
    for (index, text) in ["", "Continue with a smaller change."].iter().enumerate() {
        if !text.is_empty() {
            fake::type_text(&mut app, text);
        }
        let buffer = app_buffer_ref(&mut app, &theme, 100, 30);
        std::fs::write(
            out.join(format!("limit-{index}.ansi")),
            ansi(&buffer, &theme),
        )
        .unwrap();
    }
    press(&mut app, KeyCode::Enter);
    let key = app.open.clone().unwrap();
    fake::feed(
        &mut app,
        key.host_id.as_str(),
        key.session_id.as_str(),
        fake::update(
            key.session_id.as_str(),
            102,
            vec![
                fake::status(herder_protocol::SessionStatus::Running),
                herder_protocol::EventBody::TurnStarted {
                    turn_id: herder_protocol::TurnId::new("retry"),
                },
                herder_protocol::EventBody::ItemAdded {
                    item: herder_protocol::Item {
                        id: herder_protocol::ItemId::new("replacement"),
                        turn_id: herder_protocol::TurnId::new("retry"),
                        body: herder_protocol::ItemBody::UserMessage {
                            text: "Continue with a smaller change.".into(),
                            attachments: Vec::new(),
                        },
                    },
                },
            ],
            Vec::new(),
        ),
    );
    let buffer = app_buffer_ref(&mut app, &theme, 100, 30);
    std::fs::write(out.join("limit-2.ansi"), ansi(&buffer, &theme)).unwrap();
}
