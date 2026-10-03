//! The TUI's settings in the client profile: `tui.json`, next to its machines.

use std::path::Path;

use crate::app::App;
use crate::ui::glyphs::Glyphs;
use crate::ui::theme::Mode;

/// The client profile's file of TUI settings.
const FILE: &str = "tui.json";

/// What `:mouse` and `:glyphs` chose.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    /// Whether to ask for mouse reporting: yes unless `:mouse off` was saved.
    pub mouse: bool,
    /// The glyph set `:glyphs` chose; `None` picks by the screen's width.
    pub glyphs: Option<Glyphs>,
}

impl Settings {
    /// The settings `app` runs with.
    pub fn of(app: &App) -> Self {
        Self {
            mouse: app.mouse,
            glyphs: app.glyphs,
        }
    }
}

/// The theme `tui.json` chose. Nothing in the TUI sets it yet: edit the file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Look {
    /// The theme's name; `None` picks by the terminal.
    pub theme: Option<String>,
    /// Dark or light; `None` (`auto`) asks the terminal.
    pub mode: Option<Mode>,
}

/// What `tui.json` in `config_dir` holds; `null` when it is missing or broken.
fn saved(config_dir: &Path) -> serde_json::Value {
    std::fs::read(config_dir.join(FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .unwrap_or_default()
}

/// The theme saved in the profile in `config_dir`.
pub fn look(config_dir: &Path) -> Look {
    let saved = saved(config_dir);
    let text = |key| saved.get(key).and_then(serde_json::Value::as_str);
    Look {
        theme: text("theme").map(str::to_owned),
        mode: text("mode").and_then(Mode::parse).flatten(),
    }
}

/// The settings saved in the profile in `config_dir`; defaults for what is missing.
pub fn load(config_dir: &Path) -> Settings {
    let saved = saved(config_dir);
    Settings {
        mouse: saved
            .get("mouse")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true),
        glyphs: saved
            .get("glyphs")
            .and_then(serde_json::Value::as_str)
            .and_then(Glyphs::parse),
    }
}

/// Saves `settings` in the profile in `config_dir`, keeping what else the file holds.
pub fn save(config_dir: &Path, settings: Settings) -> std::io::Result<()> {
    let mut saved = match saved(config_dir) {
        serde_json::Value::Object(saved) => saved,
        _ => serde_json::Map::new(),
    };
    saved.insert("mouse".into(), settings.mouse.into());
    match settings.glyphs {
        Some(glyphs) => saved.insert("glyphs".into(), glyphs.name().into()),
        None => saved.remove("glyphs"),
    };
    let saved = serde_json::Value::Object(saved);
    std::fs::write(config_dir.join(FILE), format!("{saved}\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_and_default() {
        let dir = tempfile::tempdir().unwrap();
        let defaults = Settings {
            mouse: true,
            glyphs: None,
        };
        assert_eq!(load(dir.path()), defaults);
        save(dir.path(), defaults).unwrap();
        assert_eq!(load(dir.path()), defaults);
        let chosen = Settings {
            mouse: false,
            glyphs: Some(Glyphs::Unicode),
        };
        save(dir.path(), chosen).unwrap();
        assert_eq!(load(dir.path()), chosen);
    }

    #[test]
    fn the_theme_is_read_and_kept_by_saves() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(look(dir.path()), Look::default());
        std::fs::write(
            dir.path().join(FILE),
            r#"{"theme": "herder", "mode": "light", "mouse": true}"#,
        )
        .unwrap();
        let chosen = Look {
            theme: Some("herder".into()),
            mode: Some(Mode::Light),
        };
        assert_eq!(look(dir.path()), chosen);
        let settings = Settings {
            mouse: false,
            glyphs: None,
        };
        save(dir.path(), settings).unwrap();
        assert_eq!(look(dir.path()), chosen);
        assert_eq!(load(dir.path()), settings);
    }
}
