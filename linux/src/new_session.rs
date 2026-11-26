//! New Session: where it runs (a project on a machine with a clone of it, or a chat on a
//! machine) and its first prompt, as the Mac app's project picker and draft session ask them.

use std::rc::Rc;

use adw::prelude::*;
use herder_client_core::{ConnectionState, Machine};
use herder_protocol::{CommandBody, HostId, ProjectId};

/// What starts the session: the machine, the `create_session` command and the prompt.
pub type Start = Rc<dyn Fn(HostId, CommandBody, String)>;

/// Where a new session can run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub host_id: HostId,
    /// The project; `None` for a chat.
    pub project_id: Option<ProjectId>,
    pub label: String,
}

impl Target {
    pub fn command(&self) -> CommandBody {
        CommandBody::CreateSession {
            repo: None,
            project_id: self.project_id.clone(),
            chat: self.project_id.is_none(),
            branch: None,
            account_id: None,
            provider: None,
            model: None,
            permission_mode: None,
            failover_pin: None,
        }
    }
}

/// Every project on every connected machine that runs sessions, then a chat on each.
pub fn targets(machines: &[Machine]) -> Vec<Target> {
    let runs = machines
        .iter()
        .filter(|m| m.hosts.is_empty() && m.connection == ConnectionState::Connected);
    let mut targets: Vec<Target> = Vec::new();
    for machine in runs.clone() {
        for project in &machine.projects {
            targets.push(Target {
                host_id: machine.host_id.clone(),
                project_id: Some(project.project_id.clone()),
                label: format!("{} — {}", project.name, machine.name),
            });
        }
    }
    targets.sort_by_key(|target| target.label.to_lowercase());
    for machine in runs {
        targets.push(Target {
            host_id: machine.host_id.clone(),
            project_id: None,
            label: format!("Chat — {}", machine.name),
        });
    }
    targets
}

/// Asks where the session runs and its first prompt, then hands them to `start`.
pub fn dialog(parent: &impl IsA<gtk::Widget>, machines: &[Machine], start: Start) {
    let targets = targets(machines);
    if targets.is_empty() {
        let dialog = adw::AlertDialog::builder()
            .heading("No machine to start on")
            .body("Connect a machine first: a new session runs on a connected machine.")
            .build();
        dialog.add_response("close", "Close");
        dialog.present(Some(parent));
        return;
    }
    let labels: Vec<&str> = targets.iter().map(|target| target.label.as_str()).collect();
    let picker = gtk::DropDown::from_strings(&labels);
    picker.set_enable_search(true);
    let prompt = gtk::TextView::builder()
        .wrap_mode(gtk::WrapMode::WordChar)
        .top_margin(8)
        .bottom_margin(8)
        .left_margin(10)
        .right_margin(10)
        .accepts_tab(false)
        .build();
    prompt.add_css_class("new-prompt");
    let scroller = gtk::ScrolledWindow::builder()
        .min_content_height(120)
        .max_content_height(260)
        .propagate_natural_height(true)
        .child(&prompt)
        .build();
    scroller.add_css_class("new-prompt-frame");
    let fields = gtk::Box::new(gtk::Orientation::Vertical, 10);
    fields.append(&picker);
    fields.append(&scroller);
    let dialog = adw::AlertDialog::builder()
        .heading("New Session")
        .body("Pick a project, or a chat about none, and say what to do.")
        .extra_child(&fields)
        .default_response("start")
        .close_response("cancel")
        .build();
    dialog.add_responses(&[("cancel", "Cancel"), ("start", "Start")]);
    dialog.set_response_appearance("start", adw::ResponseAppearance::Suggested);
    dialog.connect_response(Some("start"), move |_, _| {
        let at = usize::try_from(picker.selected()).unwrap_or_default();
        let Some(target) = targets.get(at) else {
            return;
        };
        let buffer = prompt.buffer();
        let text = buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), false)
            .to_string();
        start(target.host_id.clone(), target.command(), text);
    });
    dialog.present(Some(parent));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lists::tests::{fleet, project};

    #[test]
    fn a_session_starts_in_a_project_on_a_connected_machine_or_as_a_chat() {
        let (mut machines, _) = fleet();
        machines[1].projects.push(project("p-cli", "cli"));
        machines[1].connection = ConnectionState::Connecting;
        let labels: Vec<String> = targets(&machines).into_iter().map(|t| t.label).collect();
        // nas is not connected; the vault runs no sessions.
        assert_eq!(labels, ["api — box", "Chat — box"]);
        let chat = targets(&machines).pop().expect("a chat");
        assert!(matches!(
            chat.command(),
            CommandBody::CreateSession {
                chat: true,
                project_id: None,
                ..
            }
        ));
    }
}
