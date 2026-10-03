//! The TUI's settings in the client profile: `tui.json`, next to its machines.

use std::path::Path;

use crate::app::App;
use crate::glyphs::Glyphs;

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

/// The settings saved in the profile in `config_dir`; defaults for what is missing.
pub fn load(config_dir: &Path) -> Settings {
    let saved = std::fs::read(config_dir.join(FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .unwrap_or_default();
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

/// Saves `settings` in the profile in `config_dir`.
pub fn save(config_dir: &Path, settings: Settings) -> std::io::Result<()> {
    let mut saved = serde_json::json!({ "mouse": settings.mouse });
    if let Some(glyphs) = settings.glyphs {
        saved["glyphs"] = glyphs.name().into();
    }
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
}
