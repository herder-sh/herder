//! The add-machine dialog: the link `herder pair` prints, or its address, fingerprint and
//! code; then the fingerprint to check before pairing; then what was paired.

use herder_client_core::{ConnectionState, Machine, PairingUri};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;
use ratatui_textarea::TextArea;

use crate::action::Action;
use crate::app::App;
use crate::machines::{AddMachine, Field, Form, Input, Step};
use crate::mouse::{Click, Hits};
use crate::palette::search_line;
use crate::ui::dialog::{Dialog, Size};
use crate::ui::hints::Hint;
use crate::ui::input::Field as TextField;
use crate::ui::select::label_width;
use crate::ui::{Ui, fit};

/// The widest label, so values line up.
const LABEL: &str = "fingerprint";

/// A machine's connection mark and its colour.
pub(super) fn connection_mark(ui: Ui, machine: &Machine) -> (&'static str, Color) {
    match &machine.connection {
        ConnectionState::Connected => (ui.glyphs.connected, ui.theme.success),
        ConnectionState::Connecting => (ui.glyphs.connecting, ui.theme.warning),
        ConnectionState::Disconnected { .. } => (ui.glyphs.disconnected, ui.theme.error),
    }
}

/// What a line of the body is.
enum Part<'a> {
    /// Text, wrapped to the body.
    Text(String, Style),
    /// A blank row.
    Blank,
    /// A form field, typed in.
    Field(Field, &'static str, &'a str, &'static str),
    /// A label and a value, the value wrapped under itself.
    Fact(&'static str, String, Style),
}

pub(super) fn draw(frame: &mut Frame, area: Rect, app: &App, add: &AddMachine, hits: &mut Hits) {
    let ui = app.ui();
    let (parts, hints): (Vec<Part>, Vec<Hint>) = match &add.step {
        Step::Edit => (
            edit(ui, &add.form, None),
            vec![
                Hint::new("enter", "next"),
                Hint::new("tab", "field"),
                Hint::new("esc", "cancel"),
            ],
        ),
        Step::Failed(failed) => (
            edit(ui, &add.form, Some(failed)),
            vec![
                Hint::new("enter", "next"),
                Hint::new("tab", "field"),
                Hint::new("esc", "cancel"),
            ],
        ),
        Step::Confirm(uri) => (
            confirm(app, ui, uri),
            vec![Hint::new("enter", "pair"), Hint::new("esc", "back")],
        ),
        Step::Pairing(uri) => (
            vec![Part::Text(
                format!(
                    "Pairing with {}{}",
                    uri.hosts.join(", "),
                    ui.glyphs.ellipsis
                ),
                ui.text(),
            )],
            vec![Hint::new("esc", "close")],
        ),
        Step::Paired(machine) => (
            vec![
                Part::Text(format!("Paired with {}.", machine.name), ui.strong()),
                Part::Blank,
                Part::Fact("host id", machine.host_id.to_string(), ui.text()),
                Part::Fact("fingerprint", grouped(&machine.fingerprint), ui.text()),
                Part::Blank,
                Part::Text(
                    "Its sessions show in the list as it connects.".into(),
                    ui.muted(),
                ),
            ],
            vec![Hint::new("enter", "done")],
        ),
    };
    let dialog = Dialog::new(ui, "add machine", Size::Medium).hints(&hints);
    // Lay out against the dialog's body width, then size the dialog to fit.
    let body_width = Size::Medium
        .width()
        .min(area.width)
        .saturating_sub(2 + 2 * crate::ui::dialog::PAD_X);
    let label = label_width(LABEL);
    let lines = layout(ui, &parts, body_width, label);
    let height = u16::try_from(lines.len()).unwrap_or(u16::MAX);
    let areas = dialog.render(area, height, frame.buffer_mut());
    super::palette::dialog_taps(hits, area, &areas);
    let buf = frame.buffer_mut();
    for (line, y) in lines.into_iter().zip(areas.body.y..areas.body.bottom()) {
        let row = Rect::new(areas.body.x, y, areas.body.width, 1);
        match line {
            Laid::Line(line) => fit(line, usize::from(row.width), ui.glyphs).render(row, buf),
            Laid::Field(field, name, value, placeholder) => {
                let mut editor: TextArea = search_line(value, placeholder);
                let focused =
                    add.form.focus == field && matches!(add.step, Step::Edit | Step::Failed(_));
                TextField::new(ui, name, &mut editor)
                    .focused(focused)
                    .render(row, buf, label);
                hits.click(row, Click::Act(Action::Machines(Input::Focus(field))));
            }
        }
    }
}

/// The form, and why the last try failed.
fn edit<'a>(ui: Ui, form: &'a Form, failed: Option<&String>) -> Vec<Part<'a>> {
    let mut parts = vec![
        Part::Text(
            "On the machine to add, run `herder pair` and paste the link it prints.".into(),
            ui.muted(),
        ),
        Part::Blank,
        Part::Field(Field::Link, "link", &form.link, "herder://pair?…"),
        Part::Blank,
        Part::Text("or what it prints, line by line:".into(), ui.muted()),
        Part::Field(Field::Host, "address", &form.host, "host[:7447]"),
        Part::Field(
            Field::Fingerprint,
            "fingerprint",
            &form.fingerprint,
            "64 hex digits",
        ),
        Part::Field(Field::Code, "code", &form.code, "one-time code"),
    ];
    if let Some(failed) = failed {
        parts.push(Part::Blank);
        parts.push(Part::Text(failed.clone(), Style::new().fg(ui.theme.error)));
    }
    parts
}

/// A body row, laid out.
enum Laid<'a> {
    Line(Line<'a>),
    Field(Field, &'static str, &'a str, &'static str),
}

/// `parts` as rows `width` wide, labels `label` wide.
fn layout<'a>(ui: Ui, parts: &[Part<'a>], width: u16, label: u16) -> Vec<Laid<'a>> {
    let width = usize::from(width).max(8);
    let mut out = Vec::new();
    for part in parts {
        match part {
            Part::Blank => out.push(Laid::Line(Line::default())),
            Part::Text(text, style) => {
                for line in textwrap::wrap(text, width) {
                    out.push(Laid::Line(Line::styled(line.into_owned(), *style)));
                }
            }
            Part::Field(field, name, value, placeholder) => {
                out.push(Laid::Field(*field, name, value, placeholder));
            }
            Part::Fact(name, value, style) => {
                let room = width.saturating_sub(usize::from(label)).max(8);
                for (at, chunk) in textwrap::wrap(value, room).into_iter().enumerate() {
                    let name = if at == 0 { *name } else { "" };
                    out.push(Laid::Line(Line::from(vec![
                        Span::styled(
                            format!("{name:<width$}", width = usize::from(label)),
                            ui.muted(),
                        ),
                        Span::styled(chunk.into_owned(), *style),
                    ])));
                }
            }
        }
    }
    out
}

/// A fingerprint in groups of four hex digits, as people compare them.
fn grouped(fingerprint: &str) -> String {
    let digits: Vec<char> = fingerprint
        .chars()
        .filter(char::is_ascii_hexdigit)
        .map(|c| c.to_ascii_lowercase())
        .collect();
    digits
        .chunks(4)
        .map(|chunk| chunk.iter().collect::<String>())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The check before pairing: where to, and the fingerprint to compare.
fn confirm<'a>(app: &App, ui: Ui, uri: &PairingUri) -> Vec<Part<'a>> {
    let fingerprint = uri.fingerprint.to_ascii_lowercase();
    let mut parts = vec![
        Part::Text("Pair with this daemon?".into(), ui.strong()),
        Part::Blank,
        Part::Fact("address", uri.hosts.join(", "), ui.text()),
        Part::Fact(
            "fingerprint",
            grouped(&fingerprint),
            Style::new()
                .fg(ui.theme.warning)
                .add_modifier(Modifier::BOLD),
        ),
        Part::Blank,
        Part::Text(
            "Check it is the fingerprint `herder pair` printed on that machine.".into(),
            ui.text(),
        ),
    ];
    if let Some(known) = app.machines.iter().find(|m| m.fingerprint == fingerprint) {
        parts.push(Part::Text(
            format!(
                "Already paired as {}: pairing again gives it a new key.",
                known.name
            ),
            ui.muted(),
        ));
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fingerprint_reads_in_groups_of_four() {
        assert_eq!(grouped("9F2C:41AB:01de"), "9f2c 41ab 01de");
    }
}
