//! The app's theme: the Mac app's palette (`apple/HerderKit/.../Design/Theme.swift`) carried
//! over to libadwaita. Like the Mac app, it is dark only for now; the light values stay
//! defined for later.
//!
//! The palette's `action` blue is libadwaita's accent, so suggested buttons, focus rings and
//! links take it; amber (`attention`) is spent on what needs the user and nothing else. The
//! surfaces are the Mac's elevation scale: the canvas under a session, a pane for the sidebar
//! and the list, a card on either. Views style their widgets only through the classes in
//! `style.css`, never with colours of their own.

use gtk::gdk;

/// A token's dark and light values, from `Theme.swift`.
const TOKENS: &[(&str, &str, &str)] = &[
    ("primary", "#5b8cff", "#3d6fe0"),
    ("secondary", "#7fa6f5", "#1c1c1c"),
    ("attention", "#e69f00", "#2e7d7a"),
    ("error", "#d55e00", "#5a5a58"),
    ("warning", "#e69f00", "#8e4a75"),
    ("success", "#009e73", "#1c1c1c"),
    ("info", "#7fa6f5", "#1c1c1c"),
    ("running", "#5382ee", "#1c1c1c"),
    ("idle", "#666666", "#b6b6b4"),
    ("child", "#9d8cf0", "#3c3c3a"),
    ("text", "#f7f7f7", "#1c1c1c"),
    ("text_secondary", "#d1d1d1", "#6a6a6a"),
    ("text_muted", "#939393", "#8c8c8a"),
    ("canvas", "#161618", "#e6e6e4"),
    ("pane", "#1b1b1e", "#ededeb"),
    ("card", "#222226", "#f4f4f2"),
    ("sheet", "#26262a", "#ededeb"),
    ("card_in_sheet", "#2e2e33", "#f7f7f5"),
    ("raised", "#313136", "#dadad8"),
    ("stroke", "#34343a", "#c9c9c7"),
    ("bubble", "#2a2a2f", "#333333"),
    ("pr_open", "#009e73", "#1c1c1c"),
    ("pr_draft", "#939393", "#8c8c8a"),
    ("pr_merged", "#cc79a7", "#5a5a58"),
    ("pr_closed", "#d55e00", "#5a5a58"),
    ("diff_added_bg", "#14291f", "#dcf2e1"),
    ("diff_removed_bg", "#33201a", "#f8dde1"),
];

/// The project tiles' fills, white initials on each (`Theme.avatars`).
pub const TILES: &[&str] = &[
    "#6a5acd", "#2f8f83", "#b5653a", "#3d7fc1", "#a8508f", "#7a8a35",
];

/// Text on the accent: white on either blue.
const ON_PRIMARY: (&str, &str) = ("#ffffff", "#ffffff");

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
    style.set_color_scheme(adw::ColorScheme::ForceDark);
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
    // libadwaita's surfaces and text, from the palette.
    let token = |name: &str| {
        TOKENS
            .iter()
            .find(|(token, ..)| *token == name)
            .map_or("", |(_, dark_value, light_value)| {
                pick((dark_value, light_value))
            })
    };
    for (adw_name, name) in [
        ("window_bg_color", "canvas"),
        ("view_bg_color", "canvas"),
        ("headerbar_bg_color", "canvas"),
        ("sidebar_bg_color", "pane"),
        ("secondary_sidebar_bg_color", "pane"),
        ("card_bg_color", "card"),
        ("popover_bg_color", "sheet"),
        ("dialog_bg_color", "sheet"),
        ("window_fg_color", "text"),
        ("view_fg_color", "text"),
        ("headerbar_fg_color", "text"),
        ("sidebar_fg_color", "text"),
        ("card_fg_color", "text"),
        ("popover_fg_color", "text"),
        ("dialog_fg_color", "text"),
        ("warning_color", "attention"),
        ("error_color", "error"),
        ("success_color", "success"),
    ] {
        let value = token(name);
        css.push_str(&format!("@define-color {adw_name} {value};\n"));
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
        assert!(css(true).contains("herder_primary #5b8cff"));
        assert!(css(true).contains("@define-color window_bg_color #161618"));
        assert!(css(false).contains("herder_primary #3d6fe0"));
        // Every token the rules use is defined.
        for word in RULES.split(|c: char| !(c.is_alphanumeric() || c == '_')) {
            if let Some(name) = word.strip_prefix("herder_") {
                assert!(TOKENS.iter().any(|(token, ..)| *token == name), "{word}");
            }
        }
    }
}
