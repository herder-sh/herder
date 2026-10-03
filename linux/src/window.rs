//! The main window: the machines in a sidebar, each with its connection state, and a content
//! pane for the selected machine.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use herder_client_core::{ConnectionState, Machine};
use herder_protocol::HostId;

/// The main window and the widgets it redraws. Cheap to clone: every field is shared.
#[derive(Clone)]
pub struct MainWindow {
    window: adw::ApplicationWindow,
    split: adw::NavigationSplitView,
    sidebar_title: adw::WindowTitle,
    /// `list`, `empty` or `error`.
    sidebar: gtk::Stack,
    list: gtk::ListBox,
    error: adw::StatusPage,
    content: adw::NavigationPage,
    placeholder: adw::StatusPage,
    machines: Rc<RefCell<Vec<Machine>>>,
    selected: Rc<RefCell<Option<HostId>>>,
}

impl MainWindow {
    /// The window, in `app` unless it is a test's.
    pub fn new(app: Option<&adw::Application>) -> Self {
        let sidebar_title = adw::WindowTitle::new("Machines", "");
        let reconnect = gtk::Button::builder()
            .icon_name("view-refresh-symbolic")
            .tooltip_text("Reconnect")
            .action_name("app.reconnect")
            .build();
        let sidebar_header = adw::HeaderBar::builder()
            .title_widget(&sidebar_title)
            .build();
        sidebar_header.pack_end(&reconnect);

        let list = gtk::ListBox::new();
        list.add_css_class("navigation-sidebar");
        let empty = adw::StatusPage::builder()
            .icon_name("network-server-symbolic")
            .title("No machines")
            .description("Pair one with <tt>herder connect &lt;link&gt;</tt>.")
            .build();
        let error = adw::StatusPage::builder()
            .icon_name("dialog-error-symbolic")
            .title("Cannot open the profile")
            .build();
        let sidebar = gtk::Stack::new();
        sidebar.add_named(
            &gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .child(&list)
                .build(),
            Some("list"),
        );
        sidebar.add_named(&empty, Some("empty"));
        sidebar.add_named(&error, Some("error"));
        sidebar.set_visible_child_name("empty");

        let sidebar_view = adw::ToolbarView::new();
        sidebar_view.add_top_bar(&sidebar_header);
        sidebar_view.set_content(Some(&sidebar));

        let placeholder = adw::StatusPage::builder()
            .icon_name("computer-symbolic")
            .title("Select a machine")
            .build();
        let content_view = adw::ToolbarView::new();
        content_view.add_top_bar(&adw::HeaderBar::new());
        content_view.set_content(Some(&placeholder));
        let content = adw::NavigationPage::builder()
            .title("Sessions")
            .tag("sessions")
            .child(&content_view)
            .build();

        let split = adw::NavigationSplitView::builder()
            .sidebar(
                &adw::NavigationPage::builder()
                    .title("Machines")
                    .tag("machines")
                    .child(&sidebar_view)
                    .build(),
            )
            .content(&content)
            .build();

        let window = adw::ApplicationWindow::builder()
            .title("herder")
            .default_width(960)
            .default_height(640)
            .content(&split)
            .build();
        let narrow = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
            adw::BreakpointConditionLengthType::MaxWidth,
            600.0,
            adw::LengthUnit::Sp,
        ));
        narrow.add_setter(&split, "collapsed", Some(&true.to_value()));
        window.add_breakpoint(narrow);
        window.set_application(app);

        let this = Self {
            window,
            split,
            sidebar_title,
            sidebar,
            list,
            error,
            content,
            placeholder,
            machines: Rc::default(),
            selected: Rc::default(),
        };
        let selected = this.clone();
        this.list.connect_row_selected(move |_, row| {
            if let Some(row) = row {
                selected.select(row.index());
            }
        });
        this
    }

    pub fn present(&self) {
        self.window.present();
    }

    /// Redraws the sidebar from the client's machines, keeping the selection.
    pub fn show_machines(&self, machines: &[Machine]) {
        *self.machines.borrow_mut() = machines.to_vec();
        self.sidebar_title.set_subtitle(&summary(machines));
        let selected = self.selected.borrow().clone();
        // Rebuilding the rows fires `row-selected` with none; the selection is restored below.
        self.list.remove_all();
        for machine in machines {
            let (mark, state) = connection(&machine.connection);
            let icon = gtk::Image::from_icon_name("media-record-symbolic");
            icon.add_css_class(mark);
            icon.set_tooltip_text(Some(&state));
            let row = adw::ActionRow::builder()
                .title(machine.name.as_str())
                .subtitle(state.as_str())
                .use_markup(false)
                .build();
            row.add_prefix(&icon);
            self.list.append(&row);
        }
        self.sidebar
            .set_visible_child_name(if machines.is_empty() { "empty" } else { "list" });
        let index = selected.and_then(|host_id| {
            machines
                .iter()
                .position(|machine| machine.host_id == host_id)
        });
        match index.and_then(|index| self.list.row_at_index(i32::try_from(index).ok()?)) {
            Some(row) => self.list.select_row(Some(&row)),
            None => self.unselect(),
        }
    }

    /// Shows why the profile could not be opened, in place of the machines.
    pub fn show_error(&self, message: &str) {
        self.sidebar_title.set_subtitle("");
        self.error
            .set_description(Some(&glib::markup_escape_text(message)));
        self.sidebar.set_visible_child_name("error");
    }

    fn select(&self, index: i32) {
        let machines = self.machines.borrow();
        let Some(machine) = usize::try_from(index)
            .ok()
            .and_then(|index| machines.get(index))
        else {
            return;
        };
        *self.selected.borrow_mut() = Some(machine.host_id.clone());
        self.content.set_title(&machine.name);
        self.placeholder
            .set_icon_name(Some("network-server-symbolic"));
        self.placeholder.set_title(&machine.name);
        self.placeholder
            .set_description(Some(&glib::markup_escape_text(&sessions(machine))));
        self.split.set_show_content(true);
    }

    fn unselect(&self) {
        *self.selected.borrow_mut() = None;
        self.content.set_title("Sessions");
        self.placeholder.set_icon_name(Some("computer-symbolic"));
        self.placeholder.set_title("Select a machine");
        self.placeholder.set_description(None);
    }
}

/// A connection's style class for its mark, and its words.
pub fn connection(state: &ConnectionState) -> (&'static str, String) {
    match state {
        ConnectionState::Connected => ("success", "connected".to_owned()),
        ConnectionState::Connecting => ("warning", "connecting".to_owned()),
        ConnectionState::Disconnected { error } => ("error", error.clone()),
    }
}

/// How many machines are connected, for the sidebar's subtitle.
pub fn summary(machines: &[Machine]) -> String {
    let connected = machines
        .iter()
        .filter(|machine| machine.connection == ConnectionState::Connected)
        .count();
    match machines.len() {
        0 => String::new(),
        total if connected == total => "all connected".to_owned(),
        total => format!("{connected} of {total} connected"),
    }
}

fn sessions(machine: &Machine) -> String {
    match machine.sessions.len() {
        1 => "1 session".to_owned(),
        count => format!("{count} sessions"),
    }
}

#[cfg(test)]
mod tests {
    use herder_protocol::{HostId, Role};

    use super::*;

    fn machine(host: &str, name: &str, connection: ConnectionState) -> Machine {
        Machine {
            host_id: HostId::new(host),
            name: name.to_owned(),
            addresses: vec!["127.0.0.1:7447".to_owned()],
            fingerprint: "ab".repeat(32),
            connection,
            role: Some(Role::Owner),
            sessions: Vec::new(),
            hosts: Vec::new(),
            projects: Vec::new(),
            accounts: Vec::new(),
            failover: Default::default(),
            terminals: Vec::new(),
            resources: None,
            session_usage: Default::default(),
        }
    }

    fn rows(window: &MainWindow) -> Vec<(String, String)> {
        let mut rows = Vec::new();
        let mut child = window.list.first_child();
        while let Some(widget) = child {
            if let Some(row) = widget.downcast_ref::<adw::ActionRow>() {
                rows.push((
                    row.title().into(),
                    row.subtitle().unwrap_or_default().into(),
                ));
            }
            child = widget.next_sibling();
        }
        rows
    }

    #[test]
    fn summary_counts_connected_machines() {
        let up = machine("h1", "box", ConnectionState::Connected);
        let down = machine(
            "h2",
            "laptop",
            ConnectionState::Disconnected {
                error: "connection refused".to_owned(),
            },
        );
        assert_eq!(summary(&[]), "");
        assert_eq!(summary(std::slice::from_ref(&up)), "all connected");
        assert_eq!(summary(&[up, down]), "1 of 2 connected");
    }

    #[gtk::test]
    fn the_window_shows_each_machine_with_its_connection() {
        adw::init().expect("libadwaita initializes");
        let window = MainWindow::new(None);
        assert_eq!(
            window.sidebar.visible_child_name().as_deref(),
            Some("empty")
        );

        window.show_machines(&[
            machine("h1", "box <1>", ConnectionState::Connected),
            machine("h2", "laptop", ConnectionState::Connecting),
            machine(
                "h3",
                "nas",
                ConnectionState::Disconnected {
                    error: "connection refused".to_owned(),
                },
            ),
        ]);
        assert_eq!(window.sidebar.visible_child_name().as_deref(), Some("list"));
        assert_eq!(window.sidebar_title.subtitle(), "1 of 3 connected");
        assert_eq!(
            rows(&window),
            [
                ("box <1>".to_owned(), "connected".to_owned()),
                ("laptop".to_owned(), "connecting".to_owned()),
                ("nas".to_owned(), "connection refused".to_owned()),
            ]
        );

        // The selection follows its machine across redraws.
        let row = window.list.row_at_index(1).expect("a second row");
        window.list.select_row(Some(&row));
        assert_eq!(window.content.title(), "laptop");
        window.show_machines(&[machine("h2", "laptop", ConnectionState::Connected)]);
        assert_eq!(window.list.selected_row().map(|row| row.index()), Some(0));
        assert_eq!(window.content.title(), "laptop");
        window.show_machines(&[]);
        assert_eq!(
            window.sidebar.visible_child_name().as_deref(),
            Some("empty")
        );
        assert_eq!(window.content.title(), "Sessions");

        window.show_error("bad <profile>");
        assert_eq!(
            window.sidebar.visible_child_name().as_deref(),
            Some("error")
        );
        assert_eq!(
            window.error.description().as_deref(),
            Some("bad &lt;profile&gt;")
        );
    }

    #[gtk::test]
    fn a_new_profile_opens_with_no_machines() {
        adw::init().expect("libadwaita initializes");
        let runtime = tokio::runtime::Runtime::new().expect("a tokio runtime");
        let _runtime = runtime.enter();
        let dir = tempfile::tempdir().expect("a temp dir");
        let client = herder_client_core::Client::open(
            dir.path().to_str().expect("a UTF-8 temp dir").to_owned(),
            "herder-gtk-test".to_owned(),
        )
        .expect("the profile opens");
        let window = MainWindow::new(None);
        window.show_machines(&client.machines());
        assert_eq!(
            window.sidebar.visible_child_name().as_deref(),
            Some("empty")
        );
    }
}
