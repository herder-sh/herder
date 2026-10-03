//! Colours: every one the TUI draws is a token of the [`Theme`].
//!
//! Token names and the theme file format are OpenCode's, so its themes port as they are,
//! plus herder's own tokens for states, usage and PRs:
//!
//! ```json
//! { "defs": { "blue": "#7aa2f7" },
//!   "theme": { "primary": { "dark": "blue", "light": "#2f5bb7" }, "info": "primary",
//!              "textMuted": 8, "background": "none" } }
//! ```
//!
//! A value is a `#rrggbb` colour, a name from `defs`, another token, an ANSI index (0–255),
//! `none` (the terminal's own colour), or an object with a value per [`Mode`]. A token a file
//! leaves out takes the token [`FALLBACKS`] names; only the core tokens are required.
//!
//! Two themes are built in: `herder` (dark and light) and `ansi`, the 16 named colours. With
//! no theme chosen, `ansi` is picked when `COLORTERM` does not promise 24-bit colour, as under
//! mosh. User themes are `<config_dir>/themes/<name>.json`.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result, bail};
use ratatui::style::Color;
use serde_json::Value;

use super::state::State;

/// The built-in `herder` theme.
const HERDER: &str = include_str!("../../themes/herder.json");
/// The built-in `ansi` theme.
const ANSI: &str = include_str!("../../themes/ansi.json");

/// The names of the built-in themes.
pub const BUILT_IN: [&str; 2] = ["herder", "ansi"];

/// Dark or light: which value of a token with one per mode a theme uses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Dark,
    Light,
}

impl Mode {
    /// The mode `name` names; `auto` is `None`.
    pub fn parse(name: &str) -> Option<Option<Self>> {
        match name {
            "dark" => Some(Some(Self::Dark)),
            "light" => Some(Some(Self::Light)),
            "auto" => Some(None),
            _ => None,
        }
    }

    /// The mode's name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
        }
    }

    /// The mode for a terminal whose `COLORFGBG` is `colorfgbg`: light on a white or
    /// light-grey background, else dark.
    pub fn detect(colorfgbg: Option<&str>) -> Self {
        match colorfgbg.and_then(|value| value.rsplit(';').next()) {
            Some("7" | "15") => Self::Light,
            _ => Self::Dark,
        }
    }
}

macro_rules! tokens {
    ($($field:ident = $name:literal $(=> $fallback:literal)?,)*) => {
        /// A resolved theme: one colour per token. Field docs give the token's name in theme
        /// files.
        #[derive(Clone, Debug, PartialEq, Eq)]
        pub struct Theme {
            $(#[doc = concat!("`", $name, "`")] pub $field: Color,)*
        }

        impl Default for Theme {
            /// Every token the terminal's own colour.
            fn default() -> Self {
                Self { $($field: Color::Reset,)* }
            }
        }

        /// Each token's name in a theme file.
        const NAMES: &[&str] = &[$($name),*];

        /// The token each optional token takes when a theme leaves it out.
        pub const FALLBACKS: &[(&str, &str)] = &[$($(($name, $fallback),)?)*];

        impl Theme {
            /// The slot of the token `name`.
            fn slot(&mut self, name: &str) -> Option<&mut Color> {
                match name {
                    $($name => Some(&mut self.$field),)*
                    _ => None,
                }
            }
        }
    };
}

tokens! {
    primary = "primary",
    secondary = "secondary",
    accent = "accent",
    error = "error",
    warning = "warning",
    success = "success",
    info = "info",
    text = "text",
    text_muted = "textMuted",
    selected_list_item_text = "selectedListItemText" => "background",
    background = "background",
    background_panel = "backgroundPanel",
    background_element = "backgroundElement",
    background_menu = "backgroundMenu" => "backgroundElement",
    border = "border",
    border_active = "borderActive" => "primary",
    border_subtle = "borderSubtle" => "border",

    diff_added = "diffAdded" => "success",
    diff_removed = "diffRemoved" => "error",
    diff_context = "diffContext" => "textMuted",
    diff_hunk_header = "diffHunkHeader" => "secondary",
    diff_highlight_added = "diffHighlightAdded" => "diffAdded",
    diff_highlight_removed = "diffHighlightRemoved" => "diffRemoved",
    diff_added_bg = "diffAddedBg" => "backgroundPanel",
    diff_removed_bg = "diffRemovedBg" => "backgroundPanel",
    diff_context_bg = "diffContextBg" => "backgroundPanel",
    diff_line_number = "diffLineNumber" => "textMuted",
    diff_added_line_number_bg = "diffAddedLineNumberBg" => "diffAddedBg",
    diff_removed_line_number_bg = "diffRemovedLineNumberBg" => "diffRemovedBg",

    markdown_text = "markdownText" => "text",
    markdown_heading = "markdownHeading" => "primary",
    markdown_link = "markdownLink" => "info",
    markdown_link_text = "markdownLinkText" => "primary",
    markdown_code = "markdownCode" => "accent",
    markdown_block_quote = "markdownBlockQuote" => "textMuted",
    markdown_emph = "markdownEmph" => "text",
    markdown_strong = "markdownStrong" => "text",
    markdown_horizontal_rule = "markdownHorizontalRule" => "border",
    markdown_list_item = "markdownListItem" => "primary",
    markdown_list_enumeration = "markdownListEnumeration" => "info",
    markdown_code_block = "markdownCodeBlock" => "text",

    syntax_comment = "syntaxComment" => "textMuted",
    syntax_keyword = "syntaxKeyword" => "accent",
    syntax_function = "syntaxFunction" => "primary",
    syntax_variable = "syntaxVariable" => "text",
    syntax_string = "syntaxString" => "success",
    syntax_number = "syntaxNumber" => "warning",
    syntax_type = "syntaxType" => "info",
    syntax_operator = "syntaxOperator" => "secondary",
    syntax_punctuation = "syntaxPunctuation" => "text",

    attention = "attention" => "accent",
    state_running = "stateRunning" => "warning",
    state_done = "stateDone" => "info",
    state_waiting = "stateWaiting" => "secondary",
    state_idle = "stateIdle" => "textMuted",
    state_error = "stateError" => "error",
    usage_low = "usageLow" => "primary",
    usage_high = "usageHigh" => "warning",
    usage_full = "usageFull" => "error",
    pr_open = "prOpen" => "success",
    pr_merged = "prMerged" => "accent",
    pr_draft = "prDraft" => "textMuted",
    pr_closed = "prClosed" => "error",
}

/// How deep references may chain before a theme counts as cyclic.
const MAX_DEPTH: usize = 16;

impl Theme {
    /// The built-in `herder` theme in `mode`.
    pub fn herder(mode: Mode) -> Self {
        // Parses: `built_in_themes_parse` checks it.
        Self::parse(HERDER, mode).unwrap_or_default()
    }

    /// The built-in `ansi` theme: the 16 named colours and the terminal's own background.
    pub fn ansi() -> Self {
        // Parses: `built_in_themes_parse` checks it.
        Self::parse(ANSI, Mode::Dark).unwrap_or_default()
    }

    /// The theme a theme file holds, in `mode`.
    pub fn parse(json: &str, mode: Mode) -> Result<Self> {
        let file: Value = serde_json::from_str(json).context("not JSON")?;
        let map = |key| match file.get(key) {
            None => Ok(HashMap::new()),
            Some(Value::Object(map)) => Ok(map.iter().map(|(k, v)| (k.as_str(), v)).collect()),
            Some(_) => bail!("`{key}` is not an object"),
        };
        let resolver = Resolver {
            defs: map("defs")?,
            tokens: map("theme")?,
            mode,
        };
        let mut theme = Self::default();
        for name in NAMES {
            let color = resolver.token(name, 0)?;
            if let Some(slot) = theme.slot(name) {
                *slot = color;
            }
        }
        Ok(theme)
    }

    /// The theme `name`: built in, or `<config_dir>/themes/<name>.json`.
    pub fn load(name: &str, mode: Mode, config_dir: &Path) -> Result<Self> {
        match name {
            "herder" => Ok(Self::herder(mode)),
            "ansi" => Ok(Self::ansi()),
            name => {
                let path = config_dir.join("themes").join(format!("{name}.json"));
                let json = std::fs::read_to_string(&path)
                    .with_context(|| format!("reading {}", path.display()))?;
                Self::parse(&json, mode).with_context(|| format!("theme {name}"))
            }
        }
    }

    /// The theme to draw with: `chosen`, else `herder` on a terminal whose `COLORTERM`
    /// promises 24-bit colour and `ansi` on any other.
    pub fn choose<'a>(chosen: Option<&'a str>, colorterm: Option<&str>) -> &'a str {
        chosen.unwrap_or(match colorterm {
            Some("truecolor" | "24bit") => "herder",
            _ => "ansi",
        })
    }

    /// The colour of `state`.
    pub fn state(&self, state: State) -> Color {
        match state {
            State::NeedsYou => self.attention,
            State::Error => self.state_error,
            State::Done => self.state_done,
            State::Running => self.state_running,
            State::Waiting | State::Moved => self.state_waiting,
            State::Idle | State::Archived | State::Unknown => self.state_idle,
        }
    }

    /// The colour of a usage bar `percent` full.
    pub fn usage(&self, percent: u8) -> Color {
        match percent {
            90.. => self.usage_full,
            70.. => self.usage_high,
            _ => self.usage_low,
        }
    }
}

/// Resolves token values against a theme file's `defs` and `theme`.
struct Resolver<'a> {
    defs: HashMap<&'a str, &'a Value>,
    tokens: HashMap<&'a str, &'a Value>,
    mode: Mode,
}

impl Resolver<'_> {
    /// The colour of the token `name`, or of its fallback when the file leaves it out.
    fn token(&self, name: &str, depth: usize) -> Result<Color> {
        if depth > MAX_DEPTH {
            bail!("`{name}` refers to itself");
        }
        if let Some(value) = self.tokens.get(name) {
            return self
                .value(value, depth + 1)
                .with_context(|| format!("token `{name}`"));
        }
        match FALLBACKS.iter().find(|(token, _)| *token == name) {
            Some((_, fallback)) => self.token(fallback, depth + 1),
            None => bail!("the theme has no `{name}`"),
        }
    }

    fn value(&self, value: &Value, depth: usize) -> Result<Color> {
        if depth > MAX_DEPTH {
            bail!("a reference refers to itself");
        }
        match value {
            Value::String(text) if text == "none" => Ok(Color::Reset),
            Value::String(text) if text.starts_with('#') => hex(text),
            Value::String(text) => match self.defs.get(text.as_str()) {
                Some(def) => self.value(def, depth + 1),
                None if NAMES.contains(&text.as_str()) => self.token(text, depth + 1),
                None => bail!("`{text}` is neither a colour, a def nor a token"),
            },
            Value::Number(number) => number
                .as_u64()
                .and_then(|index| u8::try_from(index).ok())
                .map(ansi)
                .with_context(|| format!("{number} is not an ANSI index (0-255)")),
            Value::Object(modes) => {
                let value = modes
                    .get(self.mode.name())
                    .with_context(|| format!("no `{}` value", self.mode.name()))?;
                self.value(value, depth + 1)
            }
            other => bail!("{other} is not a colour"),
        }
    }
}

/// The colour `#rrggbb` names.
fn hex(text: &str) -> Result<Color> {
    let digits = text.trim_start_matches('#');
    let channel = |at: usize| {
        digits
            .get(at..at + 2)
            .and_then(|pair| u8::from_str_radix(pair, 16).ok())
    };
    match (digits.len(), channel(0), channel(2), channel(4)) {
        (6, Some(r), Some(g), Some(b)) => Ok(Color::Rgb(r, g, b)),
        _ => bail!("`{text}` is not a #rrggbb colour"),
    }
}

/// ANSI colour `index`, by name for the 16 named ones.
fn ansi(index: u8) -> Color {
    match index {
        0 => Color::Black,
        1 => Color::Red,
        2 => Color::Green,
        3 => Color::Yellow,
        4 => Color::Blue,
        5 => Color::Magenta,
        6 => Color::Cyan,
        7 => Color::Gray,
        8 => Color::DarkGray,
        9 => Color::LightRed,
        10 => Color::LightGreen,
        11 => Color::LightYellow,
        12 => Color::LightBlue,
        13 => Color::LightMagenta,
        14 => Color::LightCyan,
        15 => Color::White,
        index => Color::Indexed(index),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_themes_parse() {
        for mode in [Mode::Dark, Mode::Light] {
            Theme::parse(HERDER, mode).unwrap();
        }
        Theme::parse(ANSI, Mode::Dark).unwrap();
        assert_ne!(Theme::herder(Mode::Dark), Theme::default());
        assert_ne!(Theme::herder(Mode::Dark), Theme::herder(Mode::Light));
    }

    #[test]
    fn herder_anchors_match_the_spec() {
        let dark = Theme::herder(Mode::Dark);
        assert_eq!(dark.primary, Color::Rgb(0x7a, 0xa2, 0xf7));
        assert_eq!(dark.attention, Color::Rgb(0xe8, 0x79, 0xc6));
        assert_eq!(dark.background, Color::Rgb(0x11, 0x12, 0x16));
        let light = Theme::herder(Mode::Light);
        assert_eq!(light.primary, Color::Rgb(0x2f, 0x5b, 0xb7));
        assert_eq!(light.background, Color::Rgb(0xff, 0xff, 0xff));
        // Filled in by fallbacks: states take the spec's defaults.
        assert_eq!(dark.state_running, dark.warning);
        assert_eq!(dark.state_done, dark.info);
        assert_eq!(dark.state_waiting, dark.secondary);
        assert_eq!(light.state_idle, light.text_muted);
        assert_eq!(dark.markdown_text, dark.text);
    }

    #[test]
    fn ansi_matches_todays_colours() {
        let ansi = Theme::ansi();
        assert_eq!(ansi.primary, Color::Cyan);
        assert_eq!(ansi.attention, Color::Magenta);
        assert_eq!(ansi.state(State::Running), Color::Yellow);
        assert_eq!(ansi.state(State::Waiting), Color::Blue);
        assert_eq!(ansi.state(State::Error), Color::Red);
        assert_eq!(ansi.text_muted, Color::DarkGray);
        assert_eq!(ansi.background, Color::Reset);
        assert_eq!(ansi.background_element, Color::Reset);
    }

    /// WCAG relative luminance of an sRGB colour.
    fn luminance(color: Color) -> f64 {
        let Color::Rgb(r, g, b) = color else {
            panic!("{color:?} is not RGB");
        };
        let linear = |c: u8| {
            let c = f64::from(c) / 255.0;
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
    }

    fn contrast(a: Color, b: Color) -> f64 {
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    #[test]
    fn herder_is_readable() {
        for mode in [Mode::Dark, Mode::Light] {
            let theme = Theme::herder(mode);
            for bg in [
                theme.background,
                theme.background_panel,
                theme.background_element,
            ] {
                let text = contrast(theme.text, bg);
                assert!(text >= 4.5, "{mode:?} text on {bg:?}: {text:.2}");
                for (name, fg) in [
                    ("primary", theme.primary),
                    ("attention", theme.attention),
                    ("error", theme.error),
                    ("warning", theme.warning),
                    ("success", theme.success),
                    ("info", theme.info),
                    ("secondary", theme.secondary),
                    ("textMuted", theme.text_muted),
                ] {
                    let ratio = contrast(fg, bg);
                    assert!(ratio >= 3.0, "{mode:?} {name} on {bg:?}: {ratio:.2}");
                }
            }
            // A solid badge: background-coloured text on the accent.
            let badge = contrast(theme.selected_list_item_text, theme.primary);
            assert!(badge >= 4.5, "{mode:?} badge: {badge:.2}");
        }
    }

    #[test]
    fn values_resolve_through_defs_tokens_modes_and_indexes() {
        let json = r##"{
            "defs": { "blue": "#0000ff" },
            "theme": {
                "primary": { "dark": "blue", "light": "#ffffff" },
                "secondary": "primary", "accent": 13, "error": 1, "warning": 3,
                "success": 2, "info": 200, "text": "none", "textMuted": 8,
                "background": "none", "backgroundPanel": "none",
                "backgroundElement": "none", "border": 8
            }
        }"##;
        let dark = Theme::parse(json, Mode::Dark).unwrap();
        assert_eq!(dark.primary, Color::Rgb(0, 0, 255));
        assert_eq!(dark.secondary, Color::Rgb(0, 0, 255));
        assert_eq!(dark.accent, Color::LightMagenta);
        assert_eq!(dark.info, Color::Indexed(200));
        // Left out: the fallbacks.
        assert_eq!(dark.attention, Color::LightMagenta);
        assert_eq!(dark.border_active, dark.primary);
        let light = Theme::parse(json, Mode::Light).unwrap();
        assert_eq!(light.primary, Color::Rgb(255, 255, 255));
    }

    #[test]
    fn broken_themes_say_why() {
        let error = |json: &str| format!("{:#}", Theme::parse(json, Mode::Dark).unwrap_err());
        assert!(error("{").contains("not JSON"));
        assert!(error(r#"{"theme": {}}"#).contains("no `primary`"));
        let cyclic = r#"{"theme": {"primary": "secondary", "secondary": "primary"}}"#;
        assert!(
            error(cyclic).contains("refers to itself"),
            "{}",
            error(cyclic)
        );
        let bad = r##"{"theme": {"primary": "#12"}}"##;
        assert!(error(bad).contains("not a #rrggbb"), "{}", error(bad));
    }

    #[test]
    fn user_themes_load_from_the_config_dir() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("themes")).unwrap();
        std::fs::write(dir.path().join("themes/mine.json"), ANSI).unwrap();
        assert_eq!(
            Theme::load("mine", Mode::Dark, dir.path()).unwrap(),
            Theme::ansi()
        );
        assert!(Theme::load("missing", Mode::Dark, dir.path()).is_err());
        assert_eq!(
            Theme::load("herder", Mode::Light, dir.path()).unwrap(),
            Theme::herder(Mode::Light)
        );
    }

    #[test]
    fn ansi_is_chosen_without_truecolor() {
        assert_eq!(Theme::choose(None, Some("truecolor")), "herder");
        assert_eq!(Theme::choose(None, Some("24bit")), "herder");
        assert_eq!(Theme::choose(None, None), "ansi");
        assert_eq!(Theme::choose(None, Some("256")), "ansi");
        assert_eq!(Theme::choose(Some("herder"), None), "herder");
        assert_eq!(Mode::detect(Some("0;15")), Mode::Light);
        assert_eq!(Mode::detect(Some("15;0")), Mode::Dark);
        assert_eq!(Mode::detect(None), Mode::Dark);
        assert_eq!(Mode::parse("auto"), Some(None));
        assert_eq!(Mode::parse("light"), Some(Some(Mode::Light)));
    }
}
