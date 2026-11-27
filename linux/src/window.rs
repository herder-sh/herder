//! The main window, laid out as the Mac app's `DesktopShell`: a sidebar with the Board, the
//! chats, the machines and the projects, each with its live counts, and the machines' dots at
//! its foot; a list pane with the section's requests and its tasks by day ([`crate::lists`]);
//! and the open session ([`crate::session_view`]) beside it. Narrower, the sidebar slides over
//! and then the list and the session take turns.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;
use herder_client_core::{ConnectionState, Machine, SessionUpdate};
use herder_protocol::{
    Answer, ApprovalDecision, CommandBody, CommandResult, Event, HostId, ProjectId, SessionStatus,
    Timestamp,
};

use crate::lists::{self, Lists, Request, RequestKind, Section, SessionKey, Task};
use crate::machines::{MachineOps, MachinesPage};
use crate::session::Session;
use crate::session_view::{Sender, SessionView};

/// How long the lists wait to redraw after a change, so a burst redraws once.
const REDRAW: Duration = Duration::from_millis(120);

/// The main window and the widgets it redraws. Cheap to clone: every field is shared.
#[derive(Clone)]
pub struct MainWindow {
    window: adw::ApplicationWindow,
    toasts: adw::ToastOverlay,
    shell: adw::OverlaySplitView,
    /// `tasks`, the list and the session; or `machines`.
    main: gtk::Stack,
    split: adw::NavigationSplitView,
    sections: gtk::ListBox,
    workspace: gtk::ListBox,
    projects: gtk::ListBox,
    board_badge: gtk::Label,
    chats_badge: gtk::Label,
    machines_badge: gtk::Label,
    footer_dots: gtk::Box,
    footer_label: gtk::Label,
    list_page: adw::NavigationPage,
    list_title: gtk::Label,
    list_status: gtk::Label,
    search: gtk::SearchEntry,
    list: gtk::Box,
    /// `list`, `empty` or `error`.
    list_stack: gtk::Stack,
    empty: adw::StatusPage,
    error: adw::StatusPage,
    empty_page: adw::NavigationPage,
    session_view: SessionView,
    machines_page: MachinesPage,
    state: Rc<RefCell<State>>,
}

/// What the window shows, as of the last change.
#[derive(Default)]
struct State {
    machines: Vec<Machine>,
    /// Every listed session as its events built it.
    sessions: HashMap<SessionKey, Session>,
    section: Section,
    /// The projects the sidebar lists, in row order.
    projects: Vec<ProjectId>,
    /// The task trees whose agents show.
    unfolded: HashSet<SessionKey>,
    archived_open: bool,
    /// A session created here, to open once its machine lists it.
    opening: Option<SessionKey>,
    /// Whether a redraw of the lists is queued.
    queued: bool,
    sender: Option<Sender>,
    ops: Option<Rc<dyn MachineOps>>,
    /// The latest events asked for of sessions opened before their journal had replayed that
    /// far; see [`MainWindow::ask_latest`].
    latest: HashMap<SessionKey, Latest>,
}

/// Where the latest events asked for of a session stand.
enum Latest {
    /// Asked for, or shown until the replay reached them: they are not asked for again.
    Asked,
    /// Folded on their own, the session view shows them until the replay reaches them.
    Shown(Box<Session>),
}

impl MainWindow {
    /// The window, in `app` unless it is a test's.
    pub fn new(app: Option<&adw::Application>) -> Self {
        // The sidebar.
        let sections = nav_list();
        let (board, board_badge) = nav_row("view-list-bullet-symbolic", "Board");
        let (chats, chats_badge) = nav_row("chat-message-new-symbolic", "Chats");
        sections.append(&board);
        sections.append(&chats);
        let workspace = nav_list();
        let (machines_row, machines_badge) = nav_row("network-server-symbolic", "Machines");
        workspace.append(&machines_row);
        let projects = nav_list();
        let footer_dots = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        let footer_label = gtk::Label::builder().xalign(0.0).build();
        footer_label.add_css_class("meta");
        let footer_content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        footer_content.append(&footer_dots);
        footer_content.append(&footer_label);
        let footer = gtk::Button::builder()
            .child(&footer_content)
            .tooltip_text("Machines")
            .build();
        footer.add_css_class("flat");
        footer.add_css_class("machines-footer");
        let new_session = gtk::Button::builder()
            .icon_name("document-new-symbolic")
            .tooltip_text("New Session")
            .action_name("win.new-session")
            .build();
        new_session.add_css_class("flat");
        let reconnect = gtk::Button::builder()
            .icon_name("view-refresh-symbolic")
            .tooltip_text("Reconnect")
            .action_name("app.reconnect")
            .build();
        reconnect.add_css_class("flat");
        let sidebar_header = adw::HeaderBar::builder()
            .show_title(false)
            .show_end_title_buttons(false)
            .build();
        let brand = gtk::Label::new(Some("herder"));
        brand.add_css_class("brand");
        sidebar_header.pack_start(&brand);
        sidebar_header.pack_end(&new_session);
        sidebar_header.pack_end(&reconnect);
        let sidebar_content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        sidebar_content.append(&sections);
        sidebar_content.append(&heading("Workspace"));
        sidebar_content.append(&workspace);
        sidebar_content.append(&heading("Projects"));
        sidebar_content.append(&projects);
        let sidebar_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&sidebar_content)
            .build();
        let sidebar_view = adw::ToolbarView::new();
        sidebar_view.add_css_class("sidebar-pane");
        sidebar_view.add_top_bar(&sidebar_header);
        sidebar_view.set_content(Some(&sidebar_scroll));
        sidebar_view.add_bottom_bar(&footer);

        // The list pane.
        let list_title = gtk::Label::builder().xalign(0.0).build();
        list_title.add_css_class("page-title");
        let list_status = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        list_status.add_css_class("meta");
        let titles = gtk::Box::new(gtk::Orientation::Vertical, 0);
        titles.set_valign(gtk::Align::Center);
        titles.append(&list_title);
        titles.append(&list_status);
        let show_sidebar = gtk::Button::builder()
            .icon_name("sidebar-show-symbolic")
            .tooltip_text("Show the Sidebar")
            .visible(false)
            .build();
        show_sidebar.add_css_class("flat");
        let add_label = gtk::Label::new(Some("New Session"));
        let add_content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        add_content.append(&gtk::Image::from_icon_name("list-add-symbolic"));
        add_content.append(&add_label);
        let add_session = gtk::Button::builder()
            .child(&add_content)
            .tooltip_text("New Session")
            .action_name("win.new-session")
            .build();
        add_session.add_css_class("header-action");
        let list_header = adw::HeaderBar::builder()
            .show_title(false)
            .show_end_title_buttons(false)
            .show_start_title_buttons(false)
            .build();
        list_header.pack_start(&show_sidebar);
        list_header.pack_start(&titles);
        list_header.pack_end(&add_session);
        let search = gtk::SearchEntry::builder()
            .placeholder_text("Search sessions, branches, projects")
            .build();
        search.add_css_class("list-search");
        let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
        list.add_css_class("task-list");
        let empty = adw::StatusPage::builder()
            .icon_name("view-list-bullet-symbolic")
            .title("Nothing here yet")
            .build();
        empty.add_css_class("compact");
        let error = adw::StatusPage::builder()
            .icon_name("dialog-error-symbolic")
            .title("Cannot open the profile")
            .build();
        let list_stack = gtk::Stack::new();
        list_stack.add_named(
            &gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .vexpand(true)
                .child(&list)
                .build(),
            Some("list"),
        );
        list_stack.add_named(&empty, Some("empty"));
        list_stack.add_named(&error, Some("error"));
        let list_content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        list_content.append(&search);
        list_content.append(&list_stack);
        let list_view = adw::ToolbarView::new();
        list_view.add_css_class("list-pane");
        list_view.add_top_bar(&list_header);
        list_view.set_content(Some(&list_content));
        let list_page = adw::NavigationPage::builder()
            .title("Board")
            .tag("list")
            .child(&list_view)
            .build();

        // The session, or what shows without one.
        let session_view = SessionView::new();
        let empty_header = adw::HeaderBar::builder().show_title(false).build();
        let empty_status = adw::StatusPage::builder()
            .icon_name("chat-message-new-symbolic")
            .title("Select a session")
            .build();
        empty_status.add_css_class("compact");
        let empty_view = adw::ToolbarView::new();
        empty_view.add_top_bar(&empty_header);
        empty_view.set_content(Some(&empty_status));
        let empty_page = adw::NavigationPage::builder()
            .title("herder")
            .tag("empty")
            .child(&empty_view)
            .build();
        let split = adw::NavigationSplitView::builder()
            .sidebar(&list_page)
            .content(&empty_page)
            .min_sidebar_width(300.0)
            .max_sidebar_width(380.0)
            .sidebar_width_fraction(0.34)
            .build();

        let machines_page = MachinesPage::new();
        let main = gtk::Stack::new();
        main.add_named(&split, Some("tasks"));
        main.add_named(machines_page.widget(), Some("machines"));
        main.add_css_class("canvas");

        let shell = adw::OverlaySplitView::builder()
            .sidebar(&sidebar_view)
            .content(&main)
            .min_sidebar_width(220.0)
            .max_sidebar_width(228.0)
            .sidebar_width_fraction(0.2)
            .build();
        shell
            .bind_property("collapsed", &show_sidebar, "visible")
            .sync_create()
            .build();
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&shell));

        let window = adw::ApplicationWindow::builder()
            .title("herder")
            .default_width(1280)
            .default_height(820)
            .width_request(360)
            .height_request(294)
            .content(&toasts)
            .build();
        let medium = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
            adw::BreakpointConditionLengthType::MaxWidth,
            1000.0,
            adw::LengthUnit::Sp,
        ));
        medium.add_setter(&shell, "collapsed", Some(&true.to_value()));
        window.add_breakpoint(medium);
        let narrow = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
            adw::BreakpointConditionLengthType::MaxWidth,
            680.0,
            adw::LengthUnit::Sp,
        ));
        narrow.add_setter(&shell, "collapsed", Some(&true.to_value()));
        narrow.add_setter(&split, "collapsed", Some(&true.to_value()));
        narrow.add_setter(&add_label, "visible", Some(&false.to_value()));
        window.add_breakpoint(narrow.clone());
        window.set_application(app);

        let this = Self {
            window,
            toasts,
            shell,
            main,
            split,
            sections,
            workspace,
            projects,
            board_badge,
            chats_badge,
            machines_badge,
            footer_dots,
            footer_label,
            list_page,
            list_title,
            list_status,
            search,
            list,
            list_stack,
            empty,
            error,
            empty_page,
            session_view,
            machines_page,
            state: Rc::default(),
        };

        let shell = this.shell.clone();
        show_sidebar.connect_clicked(move |_| shell.set_show_sidebar(true));
        let picked = this.clone();
        this.sections.connect_row_activated(move |_, row| {
            picked.pick(if row.index() == 0 {
                Section::Board
            } else {
                Section::Chats
            });
        });
        let picked = this.clone();
        this.workspace
            .connect_row_activated(move |_, _| picked.pick(Section::Machines));
        let picked = this.clone();
        this.projects.connect_row_activated(move |_, row| {
            let project = usize::try_from(row.index())
                .ok()
                .and_then(|at| picked.state.borrow().projects.get(at).cloned());
            if let Some(project) = project {
                picked.pick(Section::Project(project));
            }
        });
        let picked = this.clone();
        footer.connect_clicked(move |_| picked.pick(Section::Machines));
        let searched = this.clone();
        this.search
            .connect_search_changed(move |_| searched.show_lists());
        let opener = this.clone();
        this.session_view
            .set_opener(Rc::new(move |key| opener.open(&key)));
        let compact = this.clone();
        narrow.connect_apply(move |_| compact.session_view.set_compact(true));
        let wide = this.clone();
        narrow.connect_unapply(move |_| wide.session_view.set_compact(false));
        let creator = this.clone();
        let action = gtk::gio::SimpleAction::new("new-session", None);
        action.connect_activate(move |_, _| creator.new_session());
        this.window.add_action(&action);
        let adder = this.clone();
        this.machines_page.set_add(Rc::new(move || {
            adder.machines_page.add_machine(&adder.window)
        }));
        let adder = this.clone();
        let add_first = gtk::Button::builder().label("Add a Machine").build();
        add_first.add_css_class("pill");
        add_first.add_css_class("suggested-action");
        add_first.set_halign(gtk::Align::Center);
        add_first.connect_clicked(move |_| adder.machines_page.add_machine(&adder.window));
        this.empty.set_child(Some(&add_first));
        this.pick(Section::Board);
        this
    }

    pub fn present(&self) {
        self.window.present();
    }

    /// Where the session view's and the lists' commands go.
    pub fn set_sender(&self, sender: Sender) {
        self.state.borrow_mut().sender = Some(Rc::clone(&sender));
        self.session_view.set_sender(sender);
    }

    /// What adds, renames, reconnects and forgets machines.
    pub fn set_ops(&self, ops: Rc<dyn MachineOps>) {
        self.state.borrow_mut().ops = Some(Rc::clone(&ops));
        self.machines_page.set_ops(ops);
    }

    /// Sends `command` to `host_id`; a refusal shows as a toast.
    fn send(&self, host_id: HostId, command: CommandBody) {
        let Some(sender) = self.state.borrow().sender.clone() else {
            return;
        };
        let reply = sender(host_id, command);
        let window = self.clone();
        glib::spawn_future_local(async move {
            if let Err(message) = reply.await {
                window.toast(&message);
            }
        });
    }

    /// Shows `message` briefly over the window.
    pub fn toast(&self, message: &str) {
        self.toasts.add_toast(
            adw::Toast::builder()
                .title(glib::markup_escape_text(message))
                .timeout(6)
                .build(),
        );
    }

    /// Shows `section` in the list pane, or the machines.
    fn pick(&self, section: Section) {
        self.state.borrow_mut().section = section.clone();
        self.main
            .set_visible_child_name(if section == Section::Machines {
                "machines"
            } else {
                "tasks"
            });
        if self.shell.is_collapsed() {
            self.shell.set_show_sidebar(false);
        }
        self.mark_sidebar();
        self.show_lists();
    }

    /// Selects the sidebar row of the section shown, and only it.
    fn mark_sidebar(&self) {
        let state = self.state.borrow();
        let select = |list: &gtk::ListBox, at: Option<usize>| match at
            .and_then(|at| list.row_at_index(i32::try_from(at).ok()?))
        {
            Some(row) => list.select_row(Some(&row)),
            None => list.unselect_all(),
        };
        let (section, workspace, project) = match &state.section {
            Section::Board => (Some(0), None, None),
            Section::Chats => (Some(1), None, None),
            Section::Machines => (None, Some(0), None),
            Section::Project(id) => (
                None,
                None,
                state.projects.iter().position(|known| known == id),
            ),
        };
        select(&self.sections, section);
        select(&self.workspace, workspace);
        select(&self.projects, project);
    }

    /// Opens `key`'s session beside the list.
    pub fn open(&self, key: &SessionKey) {
        if !self.show_session(key) {
            return;
        }
        self.ask_latest(key);
        if self.main.visible_child_name().as_deref() != Some("tasks") {
            let section = self.state.borrow().section.clone();
            if section == Section::Machines {
                self.state.borrow_mut().section = Section::Board;
                self.main.set_visible_child_name("tasks");
                self.mark_sidebar();
            }
        }
        if self.split.content().as_ref() != Some(self.session_view.page()) {
            self.split.set_content(Some(self.session_view.page()));
        }
        self.split.set_show_content(true);
        self.show_lists();
    }

    /// Shows `key`'s session in the session view as it stands; whether it is listed.
    fn show_session(&self, key: &SessionKey) -> bool {
        let state = self.state.borrow();
        let Some(machine) = state.machines.iter().find(|m| m.host_id == key.host_id) else {
            return false;
        };
        if !machine
            .sessions
            .iter()
            .any(|head| head.session_id == key.session_id)
        {
            return false;
        }
        let empty = Session::default();
        let (session, latest) = match state.latest.get(key) {
            Some(Latest::Shown(latest)) => (&**latest, true),
            _ => (state.sessions.get(key).unwrap_or(&empty), false),
        };
        // The models this app's sessions of the same provider use.
        let mut recent: Vec<String> = state
            .sessions
            .values()
            .filter(|other| other.provider.is_some() && other.provider == session.provider)
            .map(|other| other.model.clone())
            .filter(|model| !model.is_empty())
            .collect();
        recent.sort();
        recent.dedup();
        self.session_view
            .show(key, session, machine, recent, latest);
        true
    }

    /// Asks once for the latest events of `key` while its journal is still on its way. A
    /// session this app holds little of replays from its start, which takes minutes for a long
    /// one on a slow link, and its latest messages come last: the view shows them meanwhile.
    fn ask_latest(&self, key: &SessionKey) {
        let sender = {
            let mut state = self.state.borrow_mut();
            if state.latest.contains_key(key) {
                return;
            }
            let held = state.sessions.get(key).map_or(0, |session| session.seq);
            let head = state
                .machines
                .iter()
                .find(|machine| machine.host_id == key.host_id)
                .and_then(|machine| {
                    machine
                        .sessions
                        .iter()
                        .find(|head| head.session_id == key.session_id)
                })
                .map_or(0, |head| head.head_seq);
            let Some(sender) = state.sender.clone().filter(|_| held < head) else {
                return;
            };
            state.latest.insert(key.clone(), Latest::Asked);
            sender
        };
        let command = CommandBody::GetLatestEvents {
            session_id: key.session_id.clone(),
        };
        let reply = sender(key.host_id.clone(), command);
        let (window, key) = (self.clone(), key.clone());
        glib::spawn_future_local(async move {
            // Not shown when it fails: the replay brings them in any case.
            if let Ok(CommandResult::LatestEvents { events }) = reply.await {
                window.show_latest(&key, events);
            }
        });
    }

    /// Shows `events`, the latest of `key`, until the replay reaches them.
    pub fn show_latest(&self, key: &SessionKey, events: Vec<Event>) {
        {
            let mut state = self.state.borrow_mut();
            let held = state.sessions.get(key).map_or(0, |session| session.seq);
            if !events.last().is_some_and(|last| last.seq > held) {
                return;
            }
            let mut latest = Session::default();
            latest.apply(&SessionUpdate {
                events,
                streaming: Vec::new(),
            });
            state
                .latest
                .insert(key.clone(), Latest::Shown(Box::new(latest)));
        }
        if self.session_view.key().as_ref() == Some(key) {
            self.show_session(key);
        }
    }

    /// Takes the client's machines, keeping what is open, and drops what the subscriptions
    /// said of sessions no longer listed.
    pub fn show_machines(&self, machines: &[Machine]) {
        let listed = lists::keys(machines);
        {
            let mut state = self.state.borrow_mut();
            state.machines = machines.to_vec();
            state.sessions.retain(|key, _| listed.contains(key));
        }
        // The open session follows its machine, or closes when no longer listed.
        if let Some(key) = self.session_view.key()
            && !self.show_session(&key)
        {
            self.session_view.close();
            self.split.set_content(Some(&self.empty_page));
        }
        let opening = self.state.borrow().opening.clone();
        if let Some(key) = opening
            && listed.contains(&key)
        {
            self.state.borrow_mut().opening = None;
            self.open(&key);
        }
        self.machines_page.show(machines);
        self.queue_lists();
    }

    /// Folds in an update of `key`'s session.
    pub fn apply(&self, key: &SessionKey, update: &SessionUpdate) {
        {
            let mut state = self.state.borrow_mut();
            // An update can outrun the machine list that drops its session.
            let listed = state.machines.iter().any(|machine| {
                machine.host_id == key.host_id
                    && machine
                        .sessions
                        .iter()
                        .any(|head| head.session_id == key.session_id)
            });
            if !listed {
                return;
            }
            let session = state.sessions.entry(key.clone()).or_default();
            session.apply(update);
            let held = session.seq;
            // The replay reached the latest events shown meanwhile.
            if let Some(Latest::Shown(latest)) = state.latest.get(key)
                && held >= latest.seq
            {
                state.latest.insert(key.clone(), Latest::Asked);
            }
        }
        if self.session_view.key().as_ref() == Some(key) {
            self.show_session(key);
        }
        self.queue_lists();
    }

    /// Shows why the profile could not be opened, in place of the lists.
    pub fn show_error(&self, message: &str) {
        self.error
            .set_description(Some(&glib::markup_escape_text(message)));
        self.list_stack.set_visible_child_name("error");
    }

    /// Redraws the lists soon, once for a burst of changes.
    fn queue_lists(&self) {
        if std::mem::replace(&mut self.state.borrow_mut().queued, true) {
            return;
        }
        let window = self.clone();
        glib::timeout_add_local_once(REDRAW, move || window.show_lists());
    }

    /// Redraws the sidebar and the list pane from the state.
    fn show_lists(&self) {
        self.state.borrow_mut().queued = false;
        self.show_sidebar();
        let state = self.state.borrow();
        let lists = Lists {
            machines: &state.machines,
            sessions: &state.sessions,
        };
        let section = state.section.clone();
        let title = match &section {
            Section::Board => "Board".to_owned(),
            Section::Chats => "Chats".to_owned(),
            Section::Machines => "Machines".to_owned(),
            Section::Project(id) => lists
                .projects()
                .into_iter()
                .find(|p| p.project_id == *id)
                .map_or_else(|| id.to_string(), |p| p.name),
        };
        self.list_title.set_label(&title);
        self.list_page.set_title(&title);
        let (waiting, running) = lists.counts(&section);
        let status = match (waiting, running) {
            _ if state.machines.is_empty() => lists::fleet_status(&state.machines),
            (0, 0) if section == Section::Board => lists::fleet_status(&state.machines),
            (0, 0) => "Nothing running".to_owned(),
            (0, running) => format!("{running} working"),
            (waiting, 0) => format!("{waiting} waiting for you"),
            (waiting, running) => format!("{waiting} waiting for you · {running} working"),
        };
        self.list_status.set_label(&status);
        if self.list_stack.visible_child_name().as_deref() == Some("error") {
            return;
        }
        let now = Timestamp::now();
        let view = lists.view(&section, &self.search.text(), day);
        let open = self.session_view.key();
        let unfolded = state.unfolded.clone();
        let archived_open = state.archived_open;
        let no_machines = state.machines.is_empty();
        drop(state);

        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        if view.requests.is_empty() && view.days.is_empty() && view.archived.is_empty() {
            if no_machines {
                self.empty.set_title("No machines yet");
                self.empty.set_description(Some(
                    "Add a machine that runs <tt>herder daemon</tt>, from a link a herder app \
                     of yours shares.",
                ));
                if let Some(child) = self.empty.child() {
                    child.set_visible(true);
                }
            } else {
                self.empty.set_title("Nothing here yet");
                self.empty
                    .set_description(Some("New sessions show here as they start."));
                if let Some(child) = self.empty.child() {
                    child.set_visible(false);
                }
            }
            self.list_stack.set_visible_child_name("empty");
            return;
        }
        self.list_stack.set_visible_child_name("list");
        if !view.requests.is_empty() {
            let heading = heading(&format!("Requests  {}", view.requests.len()));
            heading.add_css_class("attention");
            self.list.append(&heading);
            for request in &view.requests {
                self.list.append(&self.request_card(request, now));
            }
        }
        for (day, tasks) in &view.days {
            self.list.append(&heading(day));
            self.list
                .append(&self.task_card(tasks, now, open.as_ref(), &unfolded, false));
        }
        if !view.archived.is_empty() {
            let toggle = gtk::Button::builder()
                .child(&heading(&format!(
                    "{} Archived · {}",
                    if archived_open { "▾" } else { "▸" },
                    view.archived.len()
                )))
                .build();
            toggle.add_css_class("flat");
            toggle.add_css_class("section-toggle");
            let window = self.clone();
            toggle.connect_clicked(move |_| {
                {
                    let mut state = window.state.borrow_mut();
                    state.archived_open = !state.archived_open;
                }
                window.show_lists();
            });
            self.list.append(&toggle);
            if archived_open {
                self.list.append(&self.task_card(
                    &view.archived,
                    now,
                    open.as_ref(),
                    &unfolded,
                    true,
                ));
            }
        }
    }

    /// Redraws the sidebar's counts, projects and machines.
    fn show_sidebar(&self) {
        let state = self.state.borrow();
        let lists = Lists {
            machines: &state.machines,
            sessions: &state.sessions,
        };
        set_badge(&self.board_badge, lists.counts(&Section::Board));
        set_badge(&self.chats_badge, lists.counts(&Section::Chats));
        let offline = state
            .machines
            .iter()
            .filter(|m| matches!(m.connection, ConnectionState::Disconnected { .. }))
            .count();
        self.machines_badge.set_visible(offline > 0);
        self.machines_badge.set_label(&offline.to_string());
        self.machines_badge.set_css_classes(&["badge", "error"]);
        let projects = lists.projects();
        let ids: Vec<ProjectId> = projects.iter().map(|p| p.project_id.clone()).collect();
        let changed = ids != state.projects;
        let footer = footer_text(&state.machines);
        let machines = state.machines.clone();
        drop(state);
        if changed {
            self.projects.remove_all();
            for project in &projects {
                let (row, _) = project_row(&project.name);
                self.projects.append(&row);
            }
            self.state.borrow_mut().projects = ids;
            self.mark_sidebar();
        }
        for (at, project) in projects.iter().enumerate() {
            let Some(row) = i32::try_from(at)
                .ok()
                .and_then(|at| self.projects.row_at_index(at))
            else {
                continue;
            };
            if let Some(badge) = row_badge(&row) {
                set_badge(&badge, (project.waiting, project.running));
            }
        }
        while let Some(child) = self.footer_dots.first_child() {
            self.footer_dots.remove(&child);
        }
        for machine in &machines {
            let dot = gtk::Box::builder().valign(gtk::Align::Center).build();
            dot.add_css_class("dot");
            dot.add_css_class(connection_class(&machine.connection));
            dot.set_tooltip_text(Some(&format!(
                "{} · {}",
                machine.name,
                lists::connection(&machine.connection)
            )));
            self.footer_dots.append(&dot);
        }
        self.footer_label.set_label(&footer);
    }

    /// A request's card: what waits, on which session, and its answers.
    fn request_card(&self, request: &Request, now: Timestamp) -> gtk::Box {
        let card = gtk::Box::new(gtk::Orientation::Vertical, 8);
        card.add_css_class("request-card");
        let (icon, what) = match &request.kind {
            RequestKind::Approval { .. } => ("✋", "Approval needed"),
            RequestKind::Question { .. } => ("?", "Question"),
        };
        let top = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let kind = gtk::Label::builder()
            .label(format!("{icon}  {what}"))
            .xalign(0.0)
            .hexpand(true)
            .build();
        kind.add_css_class("request-kind");
        let age = gtk::Label::new(Some(&lists::age(request.since, now)));
        age.add_css_class("meta");
        top.append(&kind);
        top.append(&age);
        card.append(&top);

        let session = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        session.append(&glyph(SessionStatus::NeedsYou, true));
        let title = gtk::Label::builder()
            .label(&request.title)
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        title.add_css_class("row-title");
        let machine = gtk::Label::builder()
            .label(format!("· {}", request.machine))
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        machine.add_css_class("meta");
        session.append(&title);
        session.append(&machine);
        session.append(&gtk::Image::from_icon_name("go-next-symbolic"));
        let opener = gtk::Button::builder().child(&session).build();
        opener.add_css_class("flat");
        opener.add_css_class("request-session");
        let window = self.clone();
        let key = request.key.clone();
        opener.connect_clicked(move |_| window.open(&key));
        card.append(&opener);

        match &request.kind {
            RequestKind::Approval { id, summary } => {
                let code = gtk::Label::builder()
                    .label(summary)
                    .xalign(0.0)
                    .wrap(true)
                    .wrap_mode(gtk::pango::WrapMode::WordChar)
                    .selectable(true)
                    .build();
                code.add_css_class("request-code");
                card.append(&code);
                let buttons = gtk::Box::builder()
                    .orientation(gtk::Orientation::Horizontal)
                    .spacing(8)
                    .homogeneous(true)
                    .build();
                for (label, decision, class) in [
                    ("Deny", ApprovalDecision::Deny, "deny"),
                    ("Allow", ApprovalDecision::Allow, "allow"),
                ] {
                    let button = gtk::Button::with_label(label);
                    button.add_css_class(class);
                    let window = self.clone();
                    let key = request.key.clone();
                    let id = id.clone();
                    button.connect_clicked(move |button| {
                        button.set_sensitive(false);
                        window.send(
                            key.host_id.clone(),
                            CommandBody::AnswerApproval {
                                session_id: key.session_id.clone(),
                                approval_id: id.clone(),
                                decision,
                            },
                        );
                    });
                    buttons.append(&button);
                }
                card.append(&buttons);
            }
            RequestKind::Question {
                id,
                text,
                choices,
                multi_select,
            } => {
                let question = gtk::Label::builder()
                    .label(crate::markdown::markup(text))
                    .use_markup(true)
                    .xalign(0.0)
                    .wrap(true)
                    .wrap_mode(gtk::pango::WrapMode::WordChar)
                    .build();
                question.add_css_class("request-text");
                card.append(&question);
                if choices.is_empty() || *multi_select {
                    let answer = gtk::Button::with_label("Answer…");
                    answer.add_css_class("allow");
                    let window = self.clone();
                    let key = request.key.clone();
                    answer.connect_clicked(move |_| window.open(&key));
                    card.append(&answer);
                } else {
                    for (index, choice) in (0u32..).zip(choices) {
                        let button = gtk::Button::with_label(choice);
                        button.add_css_class("choice");
                        let window = self.clone();
                        let key = request.key.clone();
                        let id = id.clone();
                        button.connect_clicked(move |button| {
                            button.set_sensitive(false);
                            window.send(
                                key.host_id.clone(),
                                CommandBody::AnswerQuestion {
                                    session_id: key.session_id.clone(),
                                    question_id: id.clone(),
                                    answer: Answer::Choice { index },
                                    images: Vec::new(),
                                    files: Vec::new(),
                                },
                            );
                        });
                        card.append(&button);
                    }
                }
            }
        }
        card
    }

    /// A day's tasks in one card, each opening its session; a task tree's agents fold under
    /// it.
    fn task_card(
        &self,
        tasks: &[Task],
        now: Timestamp,
        open: Option<&SessionKey>,
        unfolded: &HashSet<SessionKey>,
        archived: bool,
    ) -> gtk::ListBox {
        let card = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .build();
        card.add_css_class("task-card");
        if archived {
            card.add_css_class("archived");
        }
        // What each row does, by row: open a session, or fold a task tree's agents.
        let mut actions: Vec<RowAction> = Vec::new();
        for task in tasks {
            card.append(&task_row(task, now, open == Some(&task.key), false));
            actions.push(RowAction::Open(task.key.clone()));
            if let Some(state) = task.children_state() {
                let folded = !unfolded.contains(&task.key);
                card.append(&agents_row(&task.title, task.children.len(), state, folded));
                actions.push(RowAction::Fold(task.key.clone()));
                if !folded {
                    for child in &task.children {
                        card.append(&task_row(child, now, open == Some(&child.key), true));
                        actions.push(RowAction::Open(child.key.clone()));
                    }
                }
            }
        }
        let window = self.clone();
        card.connect_row_activated(move |_, row| {
            let action = usize::try_from(row.index())
                .ok()
                .and_then(|at| actions.get(at));
            match action {
                Some(RowAction::Open(key)) => window.open(key),
                Some(RowAction::Fold(parent)) => {
                    {
                        let mut state = window.state.borrow_mut();
                        if !state.unfolded.remove(parent) {
                            state.unfolded.insert(parent.clone());
                        }
                    }
                    window.show_lists();
                }
                None => {}
            }
        });
        card
    }

    /// Asks for a project or a chat and the first prompt, then starts the session there.
    fn new_session(&self) {
        let machines = self.state.borrow().machines.clone();
        let window = self.clone();
        crate::new_session::dialog(
            &self.window,
            &machines,
            Rc::new(move |host_id, command, prompt| window.start(host_id, command, prompt)),
        );
    }

    /// Creates a session with `create` on `host_id`, prompts it and opens it once listed.
    fn start(&self, host_id: HostId, create: CommandBody, prompt: String) {
        let Some(sender) = self.state.borrow().sender.clone() else {
            return;
        };
        let window = self.clone();
        glib::spawn_future_local(async move {
            let created = sender(host_id.clone(), create).await;
            let session_id = match created {
                Ok(herder_protocol::CommandResult::SessionCreated { session_id }) => session_id,
                Ok(_) => {
                    window.toast("The machine did not create a session");
                    return;
                }
                Err(message) => {
                    window.toast(&message);
                    return;
                }
            };
            let key = SessionKey {
                host_id: host_id.clone(),
                session_id: session_id.clone(),
            };
            window.state.borrow_mut().opening = Some(key.clone());
            window.open(&key);
            if !prompt.trim().is_empty() {
                window.send(
                    host_id,
                    CommandBody::SendPrompt {
                        session_id,
                        text: prompt,
                        images: Vec::new(),
                        files: Vec::new(),
                    },
                );
            }
        });
    }
}

/// What activating a task card's row does.
enum RowAction {
    /// Opens the session.
    Open(SessionKey),
    /// Shows or hides the agents of this task tree.
    Fold(SessionKey),
}

/// The day a task's last activity falls on, in the local time zone: "Today", "Yesterday", the
/// weekday within a week, else the date.
fn day(at: Timestamp) -> String {
    let Ok(then) = glib::DateTime::from_unix_local(at.as_second()) else {
        return "Earlier".to_owned();
    };
    let Ok(now) = glib::DateTime::now_local() else {
        return "Earlier".to_owned();
    };
    let date = |time: &glib::DateTime| (time.year(), time.day_of_year());
    let days = |time: &glib::DateTime| {
        glib::DateTime::from_local(time.year(), time.month(), time.day_of_month(), 0, 0, 0.0)
            .map(|midnight| midnight.to_unix() / 86_400)
            .unwrap_or_default()
    };
    if date(&then) == date(&now) {
        return "Today".to_owned();
    }
    let ago = days(&now) - days(&then);
    let format = match ago {
        1 => return "Yesterday".to_owned(),
        2..7 => "%A",
        _ if then.year() == now.year() => "%B %-d",
        _ => "%B %-d, %Y",
    };
    then.format(format)
        .map_or_else(|_| "Earlier".to_owned(), |text| text.to_string())
}

/// What the sidebar's foot says of the machines.
fn footer_text(machines: &[Machine]) -> String {
    let offline = machines
        .iter()
        .filter(|m| matches!(m.connection, ConnectionState::Disconnected { .. }))
        .count();
    let count = match machines.len() {
        1 => "1 machine".to_owned(),
        n => format!("{n} machines"),
    };
    if offline > 0 {
        format!("{count} · {offline} offline")
    } else {
        count
    }
}

/// A connection's style class, for its dot.
pub fn connection_class(state: &ConnectionState) -> &'static str {
    match state {
        ConnectionState::Connected => "connected",
        ConnectionState::Connecting => "connecting",
        ConnectionState::Disconnected { .. } => "offline",
    }
}

fn nav_list() -> gtk::ListBox {
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::Single)
        .build();
    list.add_css_class("navigation-sidebar");
    list
}

/// A sidebar row: an icon, its name and its live counts.
fn nav_row(icon: &str, name: &str) -> (gtk::ListBoxRow, gtk::Label) {
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    content.append(&gtk::Image::from_icon_name(icon));
    content.append(
        &gtk::Label::builder()
            .label(name)
            .xalign(0.0)
            .hexpand(true)
            .build(),
    );
    let badge = gtk::Label::builder()
        .visible(false)
        .valign(gtk::Align::Center)
        .build();
    content.append(&badge);
    let row = gtk::ListBoxRow::builder().child(&content).build();
    (row, badge)
}

/// A project's sidebar row: its tile, its name and its live counts.
fn project_row(name: &str) -> (gtk::ListBoxRow, gtk::Label) {
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    content.append(&tile(name));
    content.append(
        &gtk::Label::builder()
            .label(name)
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build(),
    );
    let badge = gtk::Label::builder()
        .visible(false)
        .valign(gtk::Align::Center)
        .build();
    content.append(&badge);
    let row = gtk::ListBoxRow::builder().child(&content).build();
    (row, badge)
}

/// A row's count badge: its content's last label.
fn row_badge(row: &gtk::ListBoxRow) -> Option<gtk::Label> {
    row.child()?.last_child()?.downcast::<gtk::Label>().ok()
}

/// Shows what waits on the user in amber, else what runs in blue, else nothing.
fn set_badge(badge: &gtk::Label, (waiting, running): (usize, usize)) {
    let (count, class) = match (waiting, running) {
        (0, 0) => {
            badge.set_visible(false);
            return;
        }
        (0, running) => (running, "running"),
        (waiting, _) => (waiting, "waiting"),
    };
    badge.set_label(&count.to_string());
    badge.set_css_classes(&["badge", class]);
    badge.set_visible(true);
}

/// A project's tile: its initial on a colour of its own.
pub fn tile(name: &str) -> gtk::Label {
    let initial = name
        .chars()
        .find(|c| c.is_alphanumeric())
        .map_or_else(|| "·".to_owned(), |c| c.to_uppercase().to_string());
    let label = gtk::Label::new(Some(&initial));
    label.add_css_class("tile");
    let hash = name.bytes().fold(0usize, |hash, byte| {
        hash.wrapping_mul(31).wrapping_add(byte.into())
    });
    label.add_css_class(&format!("tile-{}", hash % crate::theme::TILES.len()));
    label.set_valign(gtk::Align::Center);
    label
}

/// A section heading, in capitals.
fn heading(text: &str) -> gtk::Label {
    let label = gtk::Label::builder()
        .label(text.to_uppercase())
        .xalign(0.0)
        .build();
    label.add_css_class("section-heading");
    label
}

/// A session's state as a glyph plus a colour, never a colour alone.
pub fn glyph(status: SessionStatus, waiting: bool) -> gtk::Label {
    let (text, class, words) = match status {
        _ if waiting => ("!", "needs", "Needs you"),
        SessionStatus::NeedsYou => ("!", "needs", "Needs you"),
        SessionStatus::Running => ("", "running", "Running"),
        SessionStatus::WaitingForCapacity => ("", "capacity", "Waiting for capacity"),
        SessionStatus::Error => ("×", "error", "Failed"),
        SessionStatus::NeedsLogin => ("!", "error", "Needs a login"),
        SessionStatus::Archived => ("", "archived", "Archived"),
        SessionStatus::Moved => ("→", "moved", "Moved"),
        SessionStatus::Idle | SessionStatus::Unknown => ("", "idle", "Idle"),
    };
    let label = gtk::Label::new(Some(text));
    label.add_css_class("glyph");
    label.add_css_class(class);
    label.set_tooltip_text(Some(words));
    label.set_valign(gtk::Align::Start);
    label.set_halign(gtk::Align::Center);
    label
}

/// A task's row: its glyph, title and age; what it is doing; its project, branch and machine.
fn task_row(task: &Task, now: Timestamp, open: bool, child: bool) -> gtk::ListBoxRow {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    row.add_css_class("task-row");
    if child {
        row.add_css_class("child");
    }
    let glyph_box = gtk::Box::builder().width_request(18).build();
    glyph_box.append(&glyph(task.status, task.waiting));
    row.append(&glyph_box);
    let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
    text.set_hexpand(true);
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let title = gtk::Label::builder()
        .label(&task.title)
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();
    title.add_css_class("row-title");
    top.append(&title);
    if let Some(at) = task.at {
        let age = gtk::Label::new(Some(&lists::age(at, now)));
        age.add_css_class("meta");
        top.append(&age);
    }
    text.append(&top);
    if !task.activity.is_empty() {
        let activity = gtk::Label::builder()
            .label(&task.activity)
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .single_line_mode(true)
            .build();
        activity.add_css_class("activity");
        if task.waiting {
            activity.add_css_class("attention");
        }
        text.append(&activity);
    }
    if !child {
        let meta = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        if let Some(project) = &task.project {
            meta.append(&tile(project));
            let name = gtk::Label::new(Some(project));
            name.add_css_class("meta");
            meta.append(&name);
        }
        if !task.branch.is_empty() {
            if task.project.is_some() {
                let dot = gtk::Label::new(Some("·"));
                dot.add_css_class("meta");
                meta.append(&dot);
            }
            let branch = gtk::Label::builder()
                .label(&task.branch)
                .xalign(0.0)
                .ellipsize(gtk::pango::EllipsizeMode::Middle)
                .build();
            branch.add_css_class("branch");
            meta.append(&branch);
        }
        let machine = gtk::Label::builder()
            .label(&task.machine)
            .xalign(1.0)
            .hexpand(true)
            .build();
        machine.add_css_class("meta");
        meta.append(&machine);
        text.append(&meta);
    }
    row.append(&text);
    let widget = gtk::ListBoxRow::builder().child(&row).build();
    if open {
        widget.add_css_class("open");
    }
    widget.set_tooltip_text(Some(&task.title));
    widget
}

/// The line under a task tree: how many agents it has and how they stand; it folds them.
fn agents_row(title: &str, count: usize, state: &str, folded: bool) -> gtk::ListBoxRow {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.add_css_class("agents-row");
    let fold = gtk::Label::new(Some(if folded { "›" } else { "⌄" }));
    fold.add_css_class("meta");
    row.append(&fold);
    let label = gtk::Label::builder()
        .label(if count == 1 {
            "1 agent".to_owned()
        } else {
            format!("{count} agents")
        })
        .xalign(0.0)
        .hexpand(true)
        .build();
    label.add_css_class("meta");
    row.append(&label);
    let state_label = gtk::Label::new(Some(state));
    state_label.add_css_class("agents-state");
    state_label.add_css_class(match state {
        "Needs you" => "attention",
        "Running" => "running",
        _ => "done",
    });
    row.append(&state_label);
    let widget = gtk::ListBoxRow::builder().child(&row).build();
    widget.set_tooltip_text(Some(&format!("Agents of {title}")));
    widget
}

#[cfg(test)]
impl MainWindow {
    pub fn set_size(&self, width: i32, height: i32) {
        self.window.set_default_size(width, height);
    }

    pub fn root(&self) -> adw::ApplicationWindow {
        self.window.clone()
    }

    /// Redraws the lists now rather than after the pause.
    pub fn flush(&self) {
        self.show_lists();
    }

    /// Picks `section`, as its sidebar row does.
    pub fn pick_for_test(&self, section: Section) {
        self.pick(section);
    }

    pub fn session_view(&self) -> &SessionView {
        &self.session_view
    }

    pub fn scroll_to_end(&self) {
        self.session_view.scroll_to_end();
    }

    /// Every label's text in the list pane, in order.
    pub fn list_texts(&self) -> Vec<String> {
        let mut texts = Vec::new();
        collect_labels(self.list.upcast_ref(), &mut texts);
        texts
    }

    /// Every label's text in the sidebar, in order.
    pub fn sidebar_texts(&self) -> Vec<String> {
        let mut texts = Vec::new();
        for list in [&self.sections, &self.workspace, &self.projects] {
            collect_labels(list.upcast_ref(), &mut texts);
        }
        collect_labels(self.footer_label.upcast_ref(), &mut texts);
        texts
    }

    pub fn list_title(&self) -> (String, String) {
        (
            self.list_title.label().to_string(),
            self.list_status.label().to_string(),
        )
    }

    pub fn search_for(&self, text: &str) {
        self.search.set_text(text);
        self.show_lists();
    }

    /// Clicks the list pane's button labelled `label`, the first there is.
    pub fn click(&self, label: &str) {
        fn find(widget: &gtk::Widget, label: &str) -> Option<gtk::Button> {
            let mut child = widget.first_child();
            while let Some(widget) = child {
                if let Some(button) = widget.downcast_ref::<gtk::Button>()
                    && button.label().as_deref() == Some(label)
                {
                    return Some(button.clone());
                }
                if let Some(found) = find(&widget, label) {
                    return Some(found);
                }
                child = widget.next_sibling();
            }
            None
        }
        find(self.list.upcast_ref(), label)
            .expect("the button")
            .emit_clicked();
    }

    /// Activates the task row titled `title`, as a click does.
    pub fn activate_row(&self, title: &str) {
        fn find(widget: &gtk::Widget, title: &str) -> Option<gtk::ListBoxRow> {
            let mut child = widget.first_child();
            while let Some(widget) = child {
                if let Some(row) = widget.downcast_ref::<gtk::ListBoxRow>()
                    && row.tooltip_text().as_deref() == Some(title)
                {
                    return Some(row.clone());
                }
                if let Some(found) = find(&widget, title) {
                    return Some(found);
                }
                child = widget.next_sibling();
            }
            None
        }
        find(self.list.upcast_ref(), title)
            .expect("the task's row")
            .emit_activate();
    }

    /// Shows the list rather than the session, as Back does on a narrow window.
    pub fn show_list(&self) {
        self.split.set_show_content(false);
    }

    /// Whether the session pane shows a session.
    pub fn shows_session(&self) -> bool {
        self.split.content().as_ref() == Some(self.session_view.page())
    }
}

#[cfg(test)]
fn collect_labels(widget: &gtk::Widget, out: &mut Vec<String>) {
    if let Some(label) = widget.downcast_ref::<gtk::Label>()
        && label.is_visible()
        && !label.label().is_empty()
    {
        out.push(label.label().to_string());
    }
    let mut child = widget.first_child();
    while let Some(widget) = child {
        if widget.is_visible() {
            collect_labels(&widget, out);
        }
        child = widget.next_sibling();
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use herder_protocol::{CommandResult, EventBody};

    use super::*;
    use crate::lists::tests::{approval, created, fleet, key, update};
    use crate::screenshots::settle;

    fn open() -> (MainWindow, Rc<RefCell<Vec<CommandBody>>>) {
        adw::init().expect("libadwaita initializes");
        let window = MainWindow::new(None);
        let sent = Rc::new(RefCell::new(Vec::new()));
        let log = Rc::clone(&sent);
        window.set_sender(Rc::new(move |_, command| {
            log.borrow_mut().push(command);
            Box::pin(async { Ok(CommandResult::Applied) })
        }));
        window.present();
        (window, sent)
    }

    /// The fleet as the window takes it: the machines, then each session's events.
    fn show_fleet(window: &MainWindow) {
        let (machines, _) = fleet();
        window.show_machines(&machines);
        window.apply(
            &key("h1", "s1"),
            &update(
                "s1",
                vec![
                    created("/src/api", "herder/api"),
                    approval("a1", "Run cargo test"),
                ],
            ),
        );
        window.apply(
            &key("h2", "s4"),
            &update("s4", vec![created("/src/api", "herder/old")]),
        );
        window.flush();
    }

    /// A session opened before its journal replayed as far as its machine lists asks once for
    /// its latest events and shows them, saying the earlier ones load, until the replay
    /// reaches them; then the whole transcript shows, each message once.
    #[gtk::test]
    fn a_session_opened_mid_replay_shows_its_latest_events_until_the_replay_reaches_them() {
        adw::init().expect("libadwaita initializes");
        let window = MainWindow::new(None);
        let said = |text: &str| {
            crate::session::tests::item(
                "m40",
                herder_protocol::ItemBody::AssistantMessage {
                    text: text.to_owned(),
                },
            )
        };
        // The whole journal: the session's start, 38 answers, then the latest.
        let mut bodies = vec![created("/src/api", "herder/api")];
        bodies.extend((2..40).map(|n| {
            crate::session::tests::item(
                &format!("m{n}"),
                herder_protocol::ItemBody::AssistantMessage {
                    text: format!("Answer {n}."),
                },
            )
        }));
        bodies.push(said("The latest answer."));
        let journal = update("s1", bodies);
        let latest = journal.events[38..].to_vec();
        let sent = Rc::new(RefCell::new(Vec::new()));
        let log = Rc::clone(&sent);
        window.set_sender(Rc::new(move |_, command| {
            log.borrow_mut().push(command);
            let events = latest.clone();
            Box::pin(async move { Ok(CommandResult::LatestEvents { events }) })
        }));
        window.present();
        let (mut machines, _) = fleet();
        machines[0].sessions[0].head_seq = 40;
        window.show_machines(&machines);
        let s1 = key("h1", "s1");
        window.apply(
            &s1,
            &SessionUpdate {
                events: journal.events[..1].to_vec(),
                streaming: Vec::new(),
            },
        );
        window.open(&s1);
        settle(Duration::from_millis(50));
        let asked = CommandBody::GetLatestEvents {
            session_id: s1.session_id.clone(),
        };
        assert_eq!(*sent.borrow(), std::slice::from_ref(&asked));
        let view = window.session_view();
        assert!(view.loading_earlier());
        let texts = view.transcript_texts();
        assert!(
            texts.iter().any(|t| t.contains("The latest answer.")),
            "{texts:?}"
        );
        assert!(!texts.iter().any(|t| t.contains("Answer 38.")), "{texts:?}");

        window.apply(
            &s1,
            &SessionUpdate {
                events: journal.events[1..].to_vec(),
                streaming: Vec::new(),
            },
        );
        window.open(&s1);
        settle(Duration::from_millis(50));
        assert!(!view.loading_earlier());
        let texts = view.transcript_texts();
        let latest = texts
            .iter()
            .filter(|t| t.contains("The latest answer."))
            .count();
        assert_eq!(latest, 1, "{texts:?}");
        assert!(texts.iter().any(|t| t.contains("Answer 38.")), "{texts:?}");
        // Asked once.
        assert_eq!(*sent.borrow(), [asked]);
    }

    #[gtk::test]
    fn the_sidebar_counts_what_waits_and_lists_projects_and_machines() {
        let (window, _) = open();
        show_fleet(&window);
        let texts = window.sidebar_texts();
        // Board: one approval waits; api: one waits, so amber wins over its running child.
        assert_eq!(
            texts,
            [
                "Board",
                "1",
                "Chats",
                "Machines",
                "A",
                "api",
                "1",
                "W",
                "web",
                "3 machines"
            ]
        );
        assert_eq!(
            window.list_title(),
            (
                "Board".to_owned(),
                "1 waiting for you · 1 working".to_owned()
            )
        );
    }

    #[gtk::test]
    fn the_board_shows_requests_then_tasks_by_day_and_archived_folded() {
        let (window, sent) = open();
        show_fleet(&window);
        let texts = window.list_texts();
        let at = |text: &str| texts.iter().position(|t| t == text);
        assert!(at("REQUESTS  1") < at("Title of s1"), "{texts:?}");
        assert!(texts.contains(&"Run cargo test".to_owned()));
        assert!(texts.contains(&"herder/api".to_owned()));
        assert!(texts.contains(&"1 agent".to_owned()));
        assert!(texts.contains(&"▸ ARCHIVED · 1".to_owned()));
        // Folded: neither the child nor the archived task shows.
        assert!(!texts.contains(&"write the tests".to_owned()));
        assert!(!texts.contains(&"Title of s4".to_owned()));
        window.activate_row("Agents of Title of s1");
        assert!(window.list_texts().contains(&"write the tests".to_owned()));
        window.activate_row("Agents of Title of s1");
        assert!(!window.list_texts().contains(&"write the tests".to_owned()));
        window.click("Allow");
        settle(Duration::from_millis(50));
        assert!(matches!(
            sent.borrow().last(),
            Some(CommandBody::AnswerApproval {
                decision: ApprovalDecision::Allow,
                ..
            })
        ));
    }

    #[gtk::test]
    fn a_task_opens_beside_the_list_and_sections_filter_it() {
        let (window, _) = open();
        show_fleet(&window);
        assert!(!window.shows_session());
        window.activate_row("Title of s1");
        assert!(window.shows_session());
        assert_eq!(window.session_view().key(), Some(key("h1", "s1")));
        window.pick_for_test(Section::Chats);
        let texts = window.list_texts();
        assert!(texts.contains(&"Title of s3".to_owned()), "{texts:?}");
        assert!(!texts.contains(&"Title of s1".to_owned()));
        window.pick_for_test(Section::Board);
        window.search_for("tests");
        assert!(window.list_texts().contains(&"Title of s1".to_owned()));
        window.search_for("nothing like it");
        assert!(!window.list_texts().contains(&"Title of s1".to_owned()));
    }

    #[gtk::test]
    fn the_list_follows_live_changes() {
        let (window, _) = open();
        show_fleet(&window);
        window.apply(
            &key("h1", "s1"),
            &update(
                "s1",
                vec![EventBody::ApprovalResolved {
                    approval_id: herder_protocol::ApprovalId::new("a1"),
                    decision: herder_protocol::ApprovalOutcome::Allow,
                    answered_by: herder_protocol::Answerer::User,
                }],
            ),
        );
        window.flush();
        let texts = window.list_texts();
        assert!(
            !texts.iter().any(|t| t.starts_with("REQUESTS")),
            "{texts:?}"
        );
        assert_eq!(window.list_title().1, "1 working");
    }

    #[gtk::test]
    fn a_new_profile_offers_to_add_a_machine() {
        let (window, _) = open();
        window.show_machines(&[]);
        window.flush();
        assert_eq!(window.list_title().1, "No machines yet");
        assert_eq!(
            window.sidebar_texts(),
            ["Board", "Chats", "Machines", "0 machines"]
        );
    }

    #[test]
    fn the_footer_counts_machines_and_those_offline() {
        let (mut machines, _) = fleet();
        assert_eq!(footer_text(&machines), "3 machines");
        machines[0].connection = ConnectionState::Disconnected {
            error: "gone".to_owned(),
        };
        assert_eq!(footer_text(&machines), "3 machines · 1 offline");
        assert_eq!(footer_text(&machines[..1]), "1 machine · 1 offline");
    }
}
