//! The secrets screen, over the fleet: a vault's sets, the secrets in each with when their
//! value was last set, the hosts each set is granted to, and the latest fetches. Over it go
//! the dialogs that change them; a value is typed masked and never shown.
//!
//! ```text
//!  secrets · vault                                      values are never shown
//!
//! ▶ coingate-prod                                              granted to devbox
//!     API_KEY        coingate merchant api                        updated 3d 2h
//!     DB_URL         primary database                             updated 5h 0m
//!   personal                                                  granted to no host
//!
//!  recent fetches
//!   2m 0s   devbox   s-7f3a   coingate-prod/API_KEY
//! ```

use herder_client_core::Machine;
use herder_protocol::{HostId, SecretAccess, SecretInfo, SecretSet, Timestamp};
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

use crate::app::App;
use crate::mouse::{Hits, Wheel};
use crate::palette::search_line;
use crate::secrets::{Dialog, Field, Form, Load, Pick, PutSecret, Secrets};
use crate::ui::dialog::{Dialog as Frame_, PAD_X, Size};
use crate::ui::hints::Hint;
use crate::ui::input::Field as TextField;
use crate::ui::list::{ListView, Row};
use crate::ui::select::label_width;
use crate::ui::{Ui, fit};

/// Widest a secret's name column gets.
const NAME: usize = 20;

/// The widest label of the secret form.
const LABEL: &str = "description";

/// Most fetches the screen shows.
const FETCHES: usize = 8;

pub(super) fn draw(frame: &mut Frame, area: Rect, app: &App, secrets: &Secrets, hits: &mut Hits) {
    let ui = app.ui();
    let compact = app.width < super::NARROW;
    let vault = app.machines.iter().find(|m| m.host_id == secrets.vault);
    let name = vault.map_or_else(|| secrets.vault.to_string(), |m| m.name.clone());
    super::clear(frame, area, ui);
    let area = super::heading(
        frame,
        area,
        ui,
        "secrets",
        &name,
        "values are never shown",
        compact,
    );
    let width = usize::from(area.width.saturating_sub(2)).max(8);
    let (sets, list) = match &secrets.list {
        Load::Asking => {
            let text = format!("Asking {name}{}", ui.glyphs.ellipsis);
            frame.render_widget(Line::styled(text, ui.muted()), Rect { height: 1, ..area });
            return dialog(frame, area, app, secrets, vault, hits);
        }
        Load::Failed(why) => {
            let mut lines = super::failure(ui, &format!("{name} has no secrets to show"), width);
            lines.push(Line::default());
            lines.extend(wrapped(why, width, ui.text()));
            lines.push(Line::default());
            lines.extend(wrapped(
                "Secrets live on a vault, encrypted under the secrets key it reads at start. \
                 Press r to ask again.",
                width,
                ui.muted(),
            ));
            lines_at(frame.buffer_mut(), area, ui, lines);
            return dialog(frame, area, app, secrets, vault, hits);
        }
        Load::Loaded((sets, list)) => (sets, list),
    };

    let picks = secrets.rows();
    let selected = secrets.selected(&picks);
    let names = list
        .iter()
        .map(|s| crate::ui::width(&s.name))
        .max()
        .unwrap_or(0)
        .min(NAME);
    let now = app.now();
    let mut rows = Vec::new();
    let mut selected_row = None;
    for (at, pick) in picks.iter().enumerate() {
        if selected == Some(at) {
            selected_row = Some(rows.len());
        }
        rows.push(match pick {
            Pick::Set(set) => set_row(ui, sets.iter().find(|s| s.name == *set), set, list, vault),
            Pick::Secret { set, name } => match secrets.secret(set, name) {
                Some(secret) => secret_row(ui, secret, names, now),
                None => Row::item(Line::default()),
            },
        });
    }
    let mut lines = Vec::new();
    if sets.is_empty() {
        lines.extend(wrapped(
            "No sets yet. Press n to create one, such as personal or prod, then a to add a \
             secret to it, and s to grant it to the hosts whose sessions may use it.",
            width,
            ui.muted(),
        ));
    }
    lines.push(Line::default());
    lines.push(Line::styled("recent fetches", ui.strong()));
    match &secrets.audit {
        Load::Asking => lines.push(Line::styled(
            format!("  {}", ui.glyphs.ellipsis),
            ui.muted(),
        )),
        Load::Failed(why) => lines.extend(super::failure(ui, why, width)),
        Load::Loaded(entries) if entries.is_empty() => {
            lines.push(Line::styled(
                "  no host has fetched a secret yet",
                ui.muted(),
            ));
        }
        Load::Loaded(entries) => {
            let hosts = entries
                .iter()
                .take(FETCHES)
                .map(|e| crate::ui::width(&host_name(vault, &e.host_id)))
                .max()
                .unwrap_or(0);
            lines.extend(
                entries
                    .iter()
                    .take(FETCHES)
                    .map(|entry| fetch_line(ui, entry, vault, hosts, now)),
            );
        }
    }

    let total = u16::try_from(rows.iter().map(Row::height).sum::<usize>()).unwrap_or(u16::MAX);
    let room = area
        .height
        .saturating_sub(u16::try_from(lines.len()).unwrap_or(u16::MAX));
    let list_area = Rect {
        height: total.min(room.max(3)).min(area.height),
        ..area
    };
    let mut offset = 0;
    ListView::new(ui, rows)
        .select(selected_row)
        .focused(secrets.dialog.is_none())
        .render(list_area, frame.buffer_mut(), &mut offset);
    if secrets.dialog.is_none() {
        hits.wheel(area, Wheel::Keys);
    }
    let rest = Rect {
        x: area.x + 1,
        y: list_area.bottom(),
        width: area.width.saturating_sub(1),
        height: area.bottom().saturating_sub(list_area.bottom()),
    };
    lines_at(frame.buffer_mut(), rest, ui, lines);
    dialog(frame, area, app, secrets, vault, hits);
}

/// A set's row: its name, and the hosts it is granted to.
fn set_row<'a>(
    ui: Ui,
    set: Option<&SecretSet>,
    name: &str,
    secrets: &[SecretInfo],
    vault: Option<&Machine>,
) -> Row<'a> {
    let granted: Vec<String> = set
        .map(|set| set.hosts.iter().map(|h| host_name(vault, h)).collect())
        .unwrap_or_default();
    let mut right = Vec::new();
    if !secrets.iter().any(|s| s.set == name) {
        right.push(Span::styled("empty", ui.muted()));
        right.push(Span::styled(ui.glyphs.separator, ui.muted()));
    }
    if granted.is_empty() {
        right.push(Span::styled("granted to no host", ui.muted()));
    } else {
        right.push(Span::styled("granted to ", ui.muted()));
        right.push(Span::styled(granted.join(", "), ui.text()));
    }
    Row::item(Line::styled(name.to_owned(), ui.strong())).right(Line::from(right))
}

/// A secret's row: its name, description and when its value was last set; never its value.
fn secret_row<'a>(ui: Ui, secret: &SecretInfo, names: usize, now: Timestamp) -> Row<'a> {
    let name = format!("  {:<names$}  ", secret.name);
    Row::item(Line::from(vec![
        Span::styled(name, ui.text()),
        Span::styled(secret.description.clone(), ui.muted()),
    ]))
    .right(Line::styled(
        format!(
            "updated {}",
            super::inbox::ago(since(now, secret.updated_at))
        ),
        ui.muted(),
    ))
}

/// A fetch: how long ago, the host, its session or purpose and the secret.
fn fetch_line<'a>(
    ui: Ui,
    entry: &SecretAccess,
    vault: Option<&Machine>,
    hosts: usize,
    now: Timestamp,
) -> Line<'a> {
    let ago = super::inbox::ago(since(now, entry.at));
    Line::from(vec![
        Span::styled(format!("  {ago:<8}"), ui.muted()),
        Span::styled(
            format!("{:<hosts$}  ", host_name(vault, &entry.host_id)),
            ui.text(),
        ),
        Span::styled(format!("{}  ", used_for(entry)), ui.muted()),
        Span::styled(format!("{}/{}", entry.set, entry.name), ui.text()),
    ])
}

/// The session a fetch was for, or the host's own purpose.
fn used_for(entry: &SecretAccess) -> &str {
    match (&entry.session_id, &entry.purpose) {
        (Some(session_id), _) => session_id.as_str(),
        (None, Some(purpose)) => purpose,
        (None, None) => "",
    }
}

fn since(now: Timestamp, then: Timestamp) -> i64 {
    now.as_second() - then.as_second()
}

/// A host by the name the vault lists it under, else its id.
fn host_name(vault: Option<&Machine>, host_id: &HostId) -> String {
    vault
        .and_then(|vault| vault.hosts.iter().find(|h| h.host_id == *host_id))
        .map_or_else(|| host_id.to_string(), |h| h.host_name.clone())
}

fn wrapped(text: &str, width: usize, style: Style) -> Vec<Line<'static>> {
    let options =
        textwrap::Options::new(width.max(8)).word_splitter(textwrap::WordSplitter::NoHyphenation);
    textwrap::wrap(text, options)
        .into_iter()
        .map(|line| Line::styled(line.into_owned(), style))
        .collect()
}

fn lines_at(buf: &mut Buffer, area: Rect, ui: Ui, lines: Vec<Line<'static>>) {
    for (line, y) in lines.into_iter().zip(area.y..area.bottom()) {
        fit(line, usize::from(area.width), ui.glyphs)
            .render(Rect::new(area.x, y, area.width, 1), buf);
    }
}

/// A body row of a dialog.
enum Part {
    Line(Line<'static>),
    /// A field: its label, what it shows, its placeholder, and whether it has the keys.
    Field(&'static str, String, &'static str, bool),
}

/// The open dialog, over the screen.
fn dialog(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    secrets: &Secrets,
    vault: Option<&Machine>,
    hits: &mut Hits,
) {
    let Some(dialog) = &secrets.dialog else {
        return;
    };
    let ui = app.ui();
    let theme = ui.theme;
    let width = usize::from(
        Size::Medium
            .width()
            .min(area.width)
            .saturating_sub(2 + 2 * PAD_X),
    )
    .max(8);
    let text = |text: &str, style: Style| -> Vec<Part> {
        wrapped(text, width, style)
            .into_iter()
            .map(Part::Line)
            .collect()
    };
    let error = |error: &Option<String>| -> Vec<Part> {
        match error {
            Some(why) => std::iter::once(Part::Line(Line::default()))
                .chain(text(why, Style::new().fg(theme.error)))
                .collect(),
            None => Vec::new(),
        }
    };
    let waiting = |sending: bool, hints: Vec<Hint>| {
        if sending {
            vec![Hint::new("esc", "close")]
        } else {
            hints
        }
    };
    let (title, parts, hints, rows, cursor): (String, Vec<Part>, Vec<Hint>, Vec<Row>, usize) =
        match dialog {
            Dialog::NewSet(Form {
                fields,
                sending,
                error: why,
            }) => {
                let mut parts = text(
                    "A set groups secrets; grant it to the hosts whose sessions may use them.",
                    ui.muted(),
                );
                parts.push(Part::Line(Line::default()));
                parts.push(Part::Field("name", fields.clone(), "prod", !sending));
                parts.extend(error(why));
                let hints = waiting(
                    *sending,
                    vec![Hint::new("enter", "create"), Hint::new("esc", "cancel")],
                );
                ("new set".to_owned(), parts, hints, Vec::new(), 0)
            }
            Dialog::Put(form) => put(ui, form, &text, &error, &waiting),
            Dialog::Hosts {
                set,
                selected,
                sending,
                error: why,
            } => {
                let hosts = secrets.hosts(vault, set);
                let granted = secrets
                    .set(set)
                    .map(|s| s.hosts.clone())
                    .unwrap_or_default();
                let mut parts = text(
                    "Every session on a granted host may use the set's secrets. A revoked host \
                     keeps what it fetched until it expires.",
                    ui.muted(),
                );
                parts.push(Part::Line(Line::default()));
                if hosts.is_empty() {
                    parts.extend(text(
                        "No host backs up to this vault yet: back one up from the fleet with b.",
                        ui.text(),
                    ));
                }
                let rows = hosts
                    .iter()
                    .map(|(host_id, name)| {
                        let on = granted.contains(host_id);
                        let (mark, style) = if on {
                            (ui.glyphs.check_pass, Style::new().fg(theme.success))
                        } else {
                            (" ", ui.muted())
                        };
                        Row::item(Line::from(vec![
                            Span::styled(mark, style),
                            Span::raw(" "),
                            Span::styled(
                                name.clone().unwrap_or_else(|| host_id.to_string()),
                                ui.text(),
                            ),
                        ]))
                        .right(Line::styled(
                            if on { "granted" } else { "not granted" },
                            ui.muted(),
                        ))
                    })
                    .collect();
                let mut after = error(why);
                if *sending {
                    after.push(Part::Line(Line::styled(
                        format!("Telling the vault{}", ui.glyphs.ellipsis),
                        ui.muted(),
                    )));
                }
                parts.extend(after);
                let hints = waiting(
                    *sending,
                    vec![
                        Hint::new("enter", "grant or revoke"),
                        Hint::new("j/k", "host"),
                        Hint::new("esc", "done"),
                    ],
                );
                (format!("hosts of {set}"), parts, hints, rows, *selected)
            }
            Dialog::Delete {
                pick,
                sending,
                error: why,
            } => {
                let (title, question, note) = match pick {
                    Pick::Set(set) => (
                        "delete set",
                        format!("Delete the set {set}?"),
                        "Its grants go with it. The vault keeps a set that holds secrets: delete \
                         them first.",
                    ),
                    Pick::Secret { set, name } => (
                        "delete secret",
                        format!("Delete {set}/{name}?"),
                        "Hosts lose it once what they fetched expires; commands that need it \
                         fail from then on.",
                    ),
                };
                let mut parts = vec![Part::Line(Line::styled(
                    question,
                    Style::new().fg(theme.warning),
                ))];
                parts.push(Part::Line(Line::default()));
                parts.extend(text(note, ui.muted()));
                parts.extend(error(why));
                let hints = waiting(
                    *sending,
                    vec![Hint::new("y", "delete"), Hint::new("n", "keep")],
                );
                (title.to_owned(), parts, hints, Vec::new(), 0)
            }
        };
    // The list goes last, after the explanation and any error.
    let list_rows = rows.len();

    let narrow = area.width < super::NARROW;
    let height = u16::try_from(parts.len() + list_rows).unwrap_or(u16::MAX);
    let areas = Frame_::new(ui, Line::raw(title), Size::Medium)
        // On a phone the button bar has them.
        .hints(if narrow { &[] } else { &hints })
        .render(area, height, frame.buffer_mut());
    super::palette::dialog_taps(hits, area, &areas);
    let body = areas.body;
    let label = label_width(LABEL);
    let buf = frame.buffer_mut();
    let mut y = body.y;
    for part in parts {
        if y >= body.bottom() {
            break;
        }
        let row = Rect::new(body.x, y, body.width, 1);
        match part {
            Part::Line(line) => fit(line, usize::from(row.width), ui.glyphs).render(row, buf),
            Part::Field(name, shown, placeholder, focused) => {
                let mut editor = search_line(&shown, placeholder);
                TextField::new(ui, name, &mut editor)
                    .focused(focused)
                    .render(row, buf, label);
            }
        }
        y += 1;
    }
    if list_rows > 0 {
        let list = Rect::new(
            body.x.saturating_sub(1),
            y,
            body.width + 2,
            u16::try_from(list_rows).unwrap_or(u16::MAX),
        )
        .intersection(areas.outer);
        let mut offset = 0;
        ListView::new(ui, rows)
            .select(Some(cursor.min(list_rows - 1)))
            .focused(true)
            .render(list, buf, &mut offset);
    }
}

/// The secret form: the set, then name, description and the value, masked.
fn put(
    ui: Ui,
    form: &Form<PutSecret>,
    text: &dyn Fn(&str, Style) -> Vec<Part>,
    error: &dyn Fn(&Option<String>) -> Vec<Part>,
    waiting: &dyn Fn(bool, Vec<Hint>) -> Vec<Hint>,
) -> (String, Vec<Part>, Vec<Hint>, Vec<Row<'static>>, usize) {
    let fields = &form.fields;
    let typing = |field: Field| !form.sending && fields.focus == field;
    let mut parts = if fields.replacing {
        text(
            "The value is never shown again: type the new one. Hosts get it once what they \
             fetched expires.",
            ui.muted(),
        )
    } else {
        text(
            "Commands get the secret as an environment variable of its name; agents see only \
             the name and description.",
            ui.muted(),
        )
    };
    parts.push(Part::Line(Line::default()));
    if fields.replacing {
        parts.push(Part::Line(Line::from(vec![
            Span::styled(
                format!("{:<w$}", "name", w = usize::from(label_width(LABEL))),
                ui.muted(),
            ),
            Span::styled(fields.name.clone(), ui.strong()),
        ])));
    } else {
        parts.push(Part::Field(
            "name",
            fields.name.clone(),
            "API_KEY",
            typing(Field::Name),
        ));
    }
    parts.push(Part::Field(
        "description",
        fields.description.clone(),
        "what it is for, on one line",
        typing(Field::Description),
    ));
    parts.push(Part::Field(
        "value",
        masked(ui, fields.value.len()),
        "typed hidden, never shown again",
        typing(Field::Value),
    ));
    parts.extend(error(&form.error));
    if form.sending {
        parts.push(Part::Line(Line::styled(
            format!("Saving{}", ui.glyphs.ellipsis),
            ui.muted(),
        )));
    }
    let hints = waiting(
        form.sending,
        vec![
            Hint::new("enter", "save"),
            Hint::new("tab", "field"),
            Hint::new("esc", "cancel"),
        ],
    );
    let title = if fields.replacing {
        format!("replace {}/{}", fields.set, fields.name)
    } else {
        format!("add a secret to {}", fields.set)
    };
    (title, parts, hints, Vec::new(), 0)
}

/// What the value field shows for `len` typed characters: a mark each, never a character of
/// the value.
pub(super) fn masked(ui: Ui, len: usize) -> String {
    ui.glyphs.bullet.repeat(len)
}
