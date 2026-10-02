//! The session list grouped by project: a heading per project with its session count and the
//! machines its sessions run on, and each session's branch and machine on its row.

use herder_protocol::ProjectId;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::ListItem;

use crate::app::App;
use crate::session::{Session, SessionKey};

/// A project's heading; `None` heads the sessions whose project is not known yet. `compact`
/// leaves out the machines.
pub(super) fn heading<'a>(
    app: &App,
    project: Option<&ProjectId>,
    width: usize,
    compact: bool,
) -> ListItem<'a> {
    let keys = sessions(app, project);
    let mut machines: Vec<&str> = Vec::new();
    for machine in &app.machines {
        if keys.iter().any(|key| key.host_id == machine.host_id) {
            machines.push(&machine.name);
        }
    }
    let (mark, name) = match project {
        Some(project) => ("◆", app.project_name(project)),
        None => ("◇", "no project yet".to_owned()),
    };
    let mut spans = vec![
        Span::styled(mark, Style::new().fg(Color::Cyan)),
        Span::styled(format!(" {name}"), super::bold()),
        Span::styled(format!(" ({})", keys.len()), super::dim()),
    ];
    if !compact && !machines.is_empty() {
        let used: usize = spans.iter().map(Span::width).sum();
        let on = format!(" · {}", machines.join(", "));
        spans.push(Span::styled(
            super::sessions::clip(&on, width.saturating_sub(used)),
            super::dim(),
        ));
    }
    ListItem::new(Line::from(spans))
}

/// The machine label of `key`'s session row: its machine's name.
pub(super) fn machine_label(app: &App, key: &SessionKey) -> Span<'static> {
    let name = app
        .machines
        .iter()
        .find(|machine| machine.host_id == key.host_id)
        .map_or_else(|| key.host_id.to_string(), |machine| machine.name.clone());
    Span::styled(format!(" {name}"), Style::new().fg(Color::Blue))
}

/// A session's name under its project's heading, which names the repo already: its branch,
/// or with `compact` the branch's last part; a child's task, as everywhere.
pub(super) fn session_name(session: &Session, compact: bool) -> String {
    if session.task.is_some() || !session.loaded {
        return session.title();
    }
    let branch = session.branch.as_str();
    match branch.rsplit('/').next() {
        Some(last) if compact => last.to_owned(),
        _ => branch.to_owned(),
    }
}

/// Every listed session of `project`.
fn sessions(app: &App, project: Option<&ProjectId>) -> Vec<SessionKey> {
    app.machines
        .iter()
        .flat_map(|machine| {
            machine.sessions.iter().map(|head| SessionKey {
                host_id: machine.host_id.clone(),
                session_id: head.session_id.clone(),
            })
        })
        .filter(|key| app.project_of(key).as_ref() == project)
        .collect()
}
