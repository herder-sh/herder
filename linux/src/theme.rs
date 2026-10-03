//! The app's theme: docs/tui-design.md §6's `herder` palette carried over to libadwaita.
//!
//! The palette's `primary` is the app's one accent: it replaces the system accent, so
//! suggested buttons, focus rings and links all take it. The state, diff and attention
//! tokens keep their TUI names as `herder_*` colours. Surfaces stay libadwaita's (window,
//! card, borders), so the app reads as a GNOME app in either scheme; views style their
//! widgets only through the classes below, never with colours of their own.

use gtk::gdk;

/// A token's dark and light values, from docs/tui-design.md §6.2.
const TOKENS: &[(&str, &str, &str)] = &[
    ("primary", "#7aa2f7", "#2f5bb7"),
    ("secondary", "#6c8ebf", "#4a6fa5"),
    ("attention", "#e879c6", "#b0307f"),
    ("error", "#f7768e", "#c4334b"),
    ("warning", "#e0af68", "#a86b00"),
    ("success", "#9ece6a", "#3f7d1f"),
    ("info", "#7dcfff", "#00739e"),
    ("text_muted", "#7f8492", "#6b6f7a"),
    ("diff_added_bg", "#1d2f24", "#dcf2e1"),
    ("diff_removed_bg", "#3a1f26", "#f8dde1"),
];

/// Text on the accent: dark on the dark scheme's light blue, white on the light's.
const ON_PRIMARY: (&str, &str) = ("#111216", "#ffffff");

/// The rules, in the tokens' names.
const RULES: &str = include_str!("style.css");

/// Loads the theme for the display's lifetime, following the light or dark scheme.
pub fn load() {
    let Some(display) = gdk::Display::default() else {
        return;
    };
    let rules = gtk::CssProvider::new();
    rules.load_from_string(RULES);
    gtk::style_context_add_provider_for_display(
        &display,
        &rules,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    let tokens = gtk::CssProvider::new();
    // Before the rules, which use the tokens: a later provider of the same priority wins.
    gtk::style_context_add_provider_for_display(
        &display,
        &tokens,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
    );
    let style = adw::StyleManager::default();
    tokens.load_from_string(&css(style.is_dark()));
    style.connect_dark_notify(move |style| tokens.load_from_string(&css(style.is_dark())));
}

/// The tokens of one scheme as CSS colours, with the accent set to `primary`.
pub fn css(dark: bool) -> String {
    let pick = |(dark_value, light_value): (&'static str, &'static str)| {
        if dark { dark_value } else { light_value }
    };
    let mut css = String::new();
    for (name, dark_value, light_value) in TOKENS {
        css.push_str(&format!(
            "@define-color herder_{name} {};\n",
            pick((dark_value, light_value))
        ));
    }
    let primary = pick((TOKENS[0].1, TOKENS[0].2));
    let on_primary = pick(ON_PRIMARY);
    css.push_str(&format!(
        "@define-color accent_color {primary};\n\
         @define-color accent_bg_color {primary};\n\
         @define-color accent_fg_color {on_primary};\n"
    ));
    // libadwaita 1.6 reads the accent from CSS variables, which GTK parses from 4.16 on.
    if gtk::check_version(4, 16, 0).is_none() {
        css.push_str(&format!(
            ":root {{ --accent-color: {primary}; --accent-bg-color: {primary}; \
             --accent-fg-color: {on_primary}; }}\n"
        ));
    }
    css
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_scheme_defines_every_token_and_the_accent() {
        for dark in [true, false] {
            let css = css(dark);
            for (name, ..) in TOKENS {
                assert!(
                    css.contains(&format!("@define-color herder_{name} #")),
                    "{name}"
                );
            }
            assert!(css.contains("@define-color accent_bg_color"));
        }
        assert!(css(true).contains("herder_primary #7aa2f7"));
        assert!(css(false).contains("herder_primary #2f5bb7"));
        // Every token the rules use is defined.
        for word in RULES.split(|c: char| !(c.is_alphanumeric() || c == '_')) {
            if let Some(name) = word.strip_prefix("herder_") {
                assert!(TOKENS.iter().any(|(token, ..)| *token == name), "{word}");
            }
        }
    }
}
