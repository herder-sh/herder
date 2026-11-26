//! The machines page, as the Mac app's Setup lists them: each machine with its connection,
//! its address, what runs on it and its herder, and what to do with it (reconnect, rename,
//! forget); a hub's hosts under it. **Add a Machine** signs this device in to the machines
//! of a `herder://fleet` link an app of yours shares, or to one machine by its address and
//! fingerprint, as `herder login` does: an app of yours approves this device with its passkey.

use std::cell::RefCell;
use std::rc::Rc;
use std::str::FromStr;

use adw::prelude::*;
use gtk::glib;
use herder_client_core::{ConnectionState, FleetLink, FleetMachine, LoginUri, Machine};
use herder_protocol::{HostId, Role, SessionStatus};

use crate::lists;

/// What the machines page does to the profile; the app's client, a fake in tests.
pub trait MachineOps {
    /// Connects to the machine again now.
    fn reconnect(&self, host_id: HostId);
    /// Shows the machine by `name` on this device.
    fn rename(&self, host_id: HostId, name: String) -> Result<(), String>;
    /// Removes the machine from this device.
    fn forget(&self, host_id: HostId) -> Result<(), String>;
    /// Signs this device in to `machine`: `shown` gets the code an app of yours approves,
    /// `done` the machine's name, or why it failed.
    fn log_in(
        &self,
        machine: FleetMachine,
        shown: Box<dyn Fn(String)>,
        done: Box<dyn FnOnce(Result<String, String>)>,
    );
}

/// The machines page. Cheap to clone.
#[derive(Clone)]
pub struct MachinesPage {
    root: adw::ToolbarView,
    status: gtk::Label,
    list: gtk::Box,
    state: Rc<RefCell<State>>,
}

#[derive(Default)]
struct State {
    machines: Vec<Machine>,
    ops: Option<Rc<dyn MachineOps>>,
    add: Option<Rc<dyn Fn()>>,
}

impl MachinesPage {
    pub fn new() -> Self {
        let title = gtk::Label::builder().label("Machines").xalign(0.0).build();
        title.add_css_class("page-title");
        let status = gtk::Label::builder().xalign(0.0).build();
        status.add_css_class("meta");
        let titles = gtk::Box::new(gtk::Orientation::Vertical, 0);
        titles.set_valign(gtk::Align::Center);
        titles.append(&title);
        titles.append(&status);
        let add = gtk::Button::builder()
            .child(&{
                let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
                content.append(&gtk::Image::from_icon_name("list-add-symbolic"));
                content.append(&gtk::Label::new(Some("Add a Machine")));
                content
            })
            .build();
        add.add_css_class("header-action");
        let header = adw::HeaderBar::builder().show_title(false).build();
        header.pack_start(&titles);
        header.pack_end(&add);
        let list = gtk::Box::new(gtk::Orientation::Vertical, 10);
        list.add_css_class("machines-list");
        let root = adw::ToolbarView::new();
        root.add_top_bar(&header);
        root.set_content(Some(
            &gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .child(&adw::Clamp::builder().maximum_size(920).child(&list).build())
                .build(),
        ));
        let this = Self {
            root,
            status,
            list,
            state: Rc::default(),
        };
        let page = this.clone();
        add.connect_clicked(move |_| {
            let add = page.state.borrow().add.clone();
            if let Some(add) = add {
                add();
            }
        });
        this
    }

    pub fn widget(&self) -> &adw::ToolbarView {
        &self.root
    }

    pub fn set_ops(&self, ops: Rc<dyn MachineOps>) {
        self.state.borrow_mut().ops = Some(ops);
    }

    /// What the header's Add a Machine does.
    pub fn set_add(&self, add: Rc<dyn Fn()>) {
        self.state.borrow_mut().add = Some(add);
    }

    /// Redraws the machines, when they changed.
    pub fn show(&self, machines: &[Machine]) {
        if self.state.borrow().machines == machines {
            return;
        }
        self.state.borrow_mut().machines = machines.to_vec();
        self.status.set_label(&lists::fleet_status(machines));
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        if machines.is_empty() {
            let empty = gtk::Label::builder()
                .label(
                    "No machines yet. Add one that runs herder daemon, from the link a herder \
                     app of yours shares.",
                )
                .wrap(true)
                .xalign(0.0)
                .build();
            empty.add_css_class("meta");
            self.list.append(&empty);
            return;
        }
        for machine in machines {
            self.list.append(&self.card(machine));
        }
    }

    fn card(&self, machine: &Machine) -> gtk::Box {
        let card = gtk::Box::new(gtk::Orientation::Vertical, 6);
        card.add_css_class("machine-card");
        let top = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        let dot = gtk::Box::builder().valign(gtk::Align::Center).build();
        dot.add_css_class("dot");
        dot.add_css_class(crate::window::connection_class(&machine.connection));
        top.append(&dot);
        let name = gtk::Label::builder()
            .label(&machine.name)
            .xalign(0.0)
            .build();
        name.add_css_class("row-title");
        top.append(&name);
        if !machine.hosts.is_empty() {
            let hub = gtk::Label::new(Some("hub"));
            hub.add_css_class("chip");
            top.append(&hub);
        }
        if let Some(role) = machine.role {
            let role = gtk::Label::new(Some(match role {
                Role::Owner => "owner",
                Role::Member => "member",
            }));
            role.add_css_class("chip");
            top.append(&role);
        }
        let spacer = gtk::Box::builder().hexpand(true).build();
        top.append(&spacer);
        for (icon, tooltip, act) in [
            ("view-refresh-symbolic", "Reconnect", Act::Reconnect),
            ("document-edit-symbolic", "Rename…", Act::Rename),
            ("user-trash-symbolic", "Forget…", Act::Forget),
        ] {
            let button = gtk::Button::builder()
                .icon_name(icon)
                .tooltip_text(tooltip)
                .build();
            button.add_css_class("flat");
            let page = self.clone();
            let host_id = machine.host_id.clone();
            let name = machine.name.clone();
            button.connect_clicked(move |button| page.act(button, act, &host_id, &name));
            top.append(&button);
        }
        card.append(&top);

        let address = machine
            .address
            .clone()
            .or_else(|| machine.addresses.first().cloned())
            .unwrap_or_default();
        let address = gtk::Label::builder()
            .label(&address)
            .xalign(0.0)
            .selectable(true)
            .build();
        address.add_css_class("branch");
        card.append(&address);

        let mut facts = vec![connection_words(machine)];
        let running = machine
            .sessions
            .iter()
            .filter(|head| head.status == SessionStatus::Running)
            .count();
        facts.push(match (machine.sessions.len(), running) {
            (1, _) => "1 session".to_owned(),
            (total, 0) => format!("{total} sessions"),
            (total, running) => format!("{running} running · {total} sessions"),
        });
        if let Some(version) = &machine.herder_version {
            facts.push(format!("herder {version}"));
        }
        if let Some(resources) = &machine.resources {
            facts.push(format!("CPU {:.0}%", resources.cpu_percent));
        }
        let facts = gtk::Label::builder()
            .label(facts.join(" · "))
            .xalign(0.0)
            .wrap(true)
            .build();
        facts.add_css_class("meta");
        card.append(&facts);
        for host in &machine.hosts {
            let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            line.set_margin_start(18);
            let dot = gtk::Box::builder().valign(gtk::Align::Center).build();
            dot.add_css_class("dot");
            dot.add_css_class(if host.online { "connected" } else { "offline" });
            line.append(&dot);
            let name = gtk::Label::builder()
                .label(format!(
                    "{} · {}",
                    host.host_name,
                    if host.online { "online" } else { "offline" }
                ))
                .xalign(0.0)
                .build();
            name.add_css_class("meta");
            line.append(&name);
            card.append(&line);
        }
        card
    }

    fn act(&self, anchor: &gtk::Button, act: Act, host_id: &HostId, name: &str) {
        let Some(ops) = self.state.borrow().ops.clone() else {
            return;
        };
        match act {
            Act::Reconnect => ops.reconnect(host_id.clone()),
            Act::Rename => {
                let entry = gtk::Entry::builder()
                    .text(name)
                    .activates_default(true)
                    .build();
                let dialog = adw::AlertDialog::builder()
                    .heading("Rename Machine")
                    .body("The name this device shows the machine by.")
                    .extra_child(&entry)
                    .default_response("rename")
                    .close_response("cancel")
                    .build();
                dialog.add_responses(&[("cancel", "Cancel"), ("rename", "Rename")]);
                dialog.set_response_appearance("rename", adw::ResponseAppearance::Suggested);
                let host_id = host_id.clone();
                dialog.connect_response(Some("rename"), move |_, _| {
                    let name = entry.text().trim().to_owned();
                    if !name.is_empty()
                        && let Err(err) = ops.rename(host_id.clone(), name)
                    {
                        glib::g_warning!("herder", "renaming: {err}");
                    }
                });
                dialog.present(Some(anchor));
            }
            Act::Forget => {
                let dialog = adw::AlertDialog::builder()
                    .heading(format!("Forget {name}?"))
                    .body(
                        "This device stops showing the machine and drops its key there. Its \
                         sessions keep running; add it again to see them.",
                    )
                    .default_response("cancel")
                    .close_response("cancel")
                    .build();
                dialog.add_responses(&[("cancel", "Cancel"), ("forget", "Forget")]);
                dialog.set_response_appearance("forget", adw::ResponseAppearance::Destructive);
                let host_id = host_id.clone();
                dialog.connect_response(Some("forget"), move |_, _| {
                    if let Err(err) = ops.forget(host_id.clone()) {
                        glib::g_warning!("herder", "forgetting: {err}");
                    }
                });
                dialog.present(Some(anchor));
            }
        }
    }

    /// Asks for a `herder://fleet` link, or an address and fingerprint, and signs in to each
    /// machine it names, showing the code an app of yours approves.
    pub fn add_machine(&self, parent: &impl IsA<gtk::Widget>) {
        let Some(ops) = self.state.borrow().ops.clone() else {
            return;
        };
        let link = gtk::Entry::builder()
            .placeholder_text("herder://fleet?…")
            .activates_default(true)
            .build();
        let address = gtk::Entry::builder().placeholder_text("host:port").build();
        let fingerprint = gtk::Entry::builder()
            .placeholder_text("SHA-256 fingerprint")
            .build();
        let by_address = gtk::Box::new(gtk::Orientation::Vertical, 6);
        by_address.append(&address);
        by_address.append(&fingerprint);
        let expander = gtk::Expander::builder()
            .label("Or by address and fingerprint")
            .child(&by_address)
            .build();
        let message = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .selectable(true)
            .visible(false)
            .build();
        let fields = gtk::Box::new(gtk::Orientation::Vertical, 10);
        fields.append(&link);
        fields.append(&expander);
        fields.append(&message);
        let dialog = adw::AlertDialog::builder()
            .heading("Add a Machine")
            .body(
                "Paste the link an app of yours shares (You › Share), or the address and \
                 fingerprint `herder daemon` prints. An app of yours then approves this device \
                 with its passkey.",
            )
            .extra_child(&fields)
            .default_response("sign-in")
            .close_response("close")
            .build();
        dialog.add_responses(&[("close", "Close"), ("sign-in", "Sign In")]);
        dialog.set_response_appearance("sign-in", adw::ResponseAppearance::Suggested);
        dialog.set_response_enabled("sign-in", false);
        let enable = {
            let dialog = dialog.clone();
            let (link, address, fingerprint) = (link.clone(), address.clone(), fingerprint.clone());
            move || {
                dialog.set_response_enabled(
                    "sign-in",
                    targets(&link.text(), &address.text(), &fingerprint.text()).is_ok(),
                );
            }
        };
        for entry in [&link, &address, &fingerprint] {
            let enable = enable.clone();
            entry.connect_changed(move |_| enable());
        }
        let page = self.clone();
        dialog.connect_response(Some("sign-in"), move |dialog, _| {
            // The dialog stays to show the code: a response closes it unless stopped.
            dialog.set_can_close(false);
            let machines = match targets(&link.text(), &address.text(), &fingerprint.text()) {
                Ok(machines) => machines,
                Err(err) => {
                    show(&message, &err, true);
                    dialog.set_can_close(true);
                    return;
                }
            };
            dialog.set_response_enabled("sign-in", false);
            show(&message, "Connecting…", false);
            page.sign_in(&ops, dialog, &message, machines);
        });
        let closing = dialog.clone();
        dialog.connect_response(Some("close"), move |_, _| {
            closing.set_can_close(true);
            closing.force_close();
        });
        dialog.present(Some(parent));
    }

    /// Signs in to each of `machines` in turn, then lets the dialog close.
    fn sign_in(
        &self,
        ops: &Rc<dyn MachineOps>,
        dialog: &adw::AlertDialog,
        message: &gtk::Label,
        mut machines: Vec<FleetMachine>,
    ) {
        if machines.is_empty() {
            dialog.set_can_close(true);
            return;
        }
        let machine = machines.remove(0);
        let shown = {
            let message = message.clone();
            let name = machine.name.clone();
            Box::new(move |code: String| {
                let on = name
                    .as_ref()
                    .map(|name| format!(" to sign in to {name}"))
                    .unwrap_or_default();
                show(
                    &message,
                    &format!(
                        "Approve this device{on} in a herder app of yours, with the code \
                         {code}. Check the app shows the same code. No app? Run /approve {code} \
                         where your passkey is."
                    ),
                    false,
                );
            })
        };
        let done = {
            let page = self.clone();
            let ops = Rc::clone(ops);
            let dialog = dialog.clone();
            let message = message.clone();
            Box::new(move |result: Result<String, String>| match result {
                Ok(name) => {
                    show(&message, &format!("Signed in to {name}."), false);
                    if machines.is_empty() {
                        dialog.set_can_close(true);
                        dialog.set_response_label("close", "Done");
                    } else {
                        page.sign_in(&ops, &dialog, &message, machines);
                    }
                }
                Err(err) => {
                    show(&message, &err, true);
                    dialog.set_can_close(true);
                    dialog.set_response_enabled("sign-in", true);
                }
            })
        };
        ops.log_in(machine, shown, done);
    }
}

#[derive(Clone, Copy)]
enum Act {
    Reconnect,
    Rename,
    Forget,
}

fn show(message: &gtk::Label, text: &str, error: bool) {
    message.set_label(text);
    message.set_visible(true);
    if error {
        message.add_css_class("error");
    } else {
        message.remove_css_class("error");
    }
}

/// The machines to sign in to: a fleet link's, a login link's, else the one at `address`.
pub fn targets(link: &str, address: &str, fingerprint: &str) -> Result<Vec<FleetMachine>, String> {
    let link = link.trim();
    if !link.is_empty() {
        if let Ok(fleet) = FleetLink::from_str(link) {
            return Ok(fleet.machines);
        }
        return LoginUri::from_str(link)
            .map(|login| {
                vec![FleetMachine {
                    hosts: login.hosts,
                    fingerprint: login.fingerprint,
                    name: None,
                }]
            })
            .map_err(|err| err.to_string());
    }
    let (address, fingerprint) = (address.trim(), fingerprint.trim());
    if address.is_empty() || fingerprint.is_empty() {
        return Err("Paste a link, or give an address and a fingerprint.".to_owned());
    }
    let fingerprint: String = fingerprint
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .collect::<String>()
        .to_lowercase();
    if fingerprint.len() != 64 {
        return Err("A fingerprint is 64 hex digits.".to_owned());
    }
    Ok(vec![FleetMachine {
        hosts: vec![address.to_owned()],
        fingerprint,
        name: None,
    }])
}

/// A machine's connection in words, with its round trip when connected.
fn connection_words(machine: &Machine) -> String {
    match (&machine.connection, machine.quality.last_rtt_ms) {
        (ConnectionState::Connected, Some(rtt)) => format!("Connected · {rtt} ms"),
        (ConnectionState::Disconnected { error }, _) if !error.is_empty() => {
            format!("Offline: {error}")
        }
        (state, _) => lists::connection(state).to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_machine_is_named_by_a_link_or_its_address_and_fingerprint() {
        let fingerprint = "ab".repeat(32);
        let machines = targets("", "box.lan:7447", &fingerprint.to_uppercase()).expect("one");
        assert_eq!(machines.len(), 1);
        assert_eq!(machines[0].hosts, ["box.lan:7447"]);
        assert_eq!(machines[0].fingerprint, fingerprint);
        assert!(targets("", "box.lan:7447", "abc").is_err());
        assert!(targets("", "", "").is_err());
        assert!(targets("not a link", "", "").is_err());
    }
}
