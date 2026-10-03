//! The main window: the machines in a sidebar, each with its connection state and a vault's
//! hosts under it, online or offline; and the sessions of the selected one, or of all, grouped
//! by project or by machine ([`crate::lists`]). Activating a session opens it
//! ([`crate::session_view`]) over the list. Narrow, the sidebar, the list and the session are
//! pages of one stack and rows show less, as the TUI's compact rows do.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};
use herder_client_core::{ConnectionState, Machine, SessionUpdate};
use herder_protocol::{CiStatus, Mergeable, PrState, PullRequest, ReviewStatus, SessionStatus};

use crate::lists::{self, Grouping, Lists, Scope, SessionKey, Summary};
use crate::session::Session;
use crate::session_view::{Sender, SessionView};

/// Most PRs a row names; the rest are counted.
const ROW_PRS: usize = 2;

/// Pixels a child row is indented per level.
const INDENT: i32 = 18;

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
    /// The session list, and an open session over it.
    nav: adw::NavigationView,
    sessions_page: adw::NavigationPage,
    session_view: SessionView,
    /// `sessions` or `empty`.
    content_stack: gtk::Stack,
    /// The groups of the session list.
    groups: gtk::Box,
    by_machine: gtk::ToggleButton,
    state: Rc<RefCell<State>>,
}

/// What the window shows, as of the last change.
#[derive(Default)]
struct State {
    machines: Vec<Machine>,
    summaries: HashMap<SessionKey, Summary>,
    /// Every listed session as its events built it, for the session view.
    sessions: HashMap<SessionKey, Session>,
    /// What each sidebar row selects, in row order.
    scopes: Vec<Scope>,
    scope: Scope,
    grouping: Grouping,
    compact: bool,
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

        let by_machine = gtk::ToggleButton::builder()
            .icon_name("network-server-symbolic")
            .tooltip_text("Group by machine")
            .build();
        let content_header = adw::HeaderBar::new();
        content_header.pack_end(&by_machine);
        let groups = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(18)
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(12)
            .margin_end(12)
            .build();
        let no_sessions = adw::StatusPage::builder()
            .icon_name("computer-symbolic")
            .title("No sessions")
            .build();
        let content_stack = gtk::Stack::new();
        content_stack.add_named(
            &gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .child(
                    &adw::Clamp::builder()
                        .maximum_size(900)
                        .child(&groups)
                        .build(),
                )
                .build(),
            Some("sessions"),
        );
        content_stack.add_named(&no_sessions, Some("empty"));
        content_stack.set_visible_child_name("empty");
        let content_view = adw::ToolbarView::new();
        content_view.add_top_bar(&content_header);
        content_view.set_content(Some(&content_stack));
        let sessions_page = adw::NavigationPage::builder()
            .title("Sessions")
            .tag("sessions")
            .child(&content_view)
            .build();
        let nav = adw::NavigationView::new();
        nav.add(&sessions_page);
        let session_view = SessionView::new();
        let content = adw::NavigationPage::builder()
            .title("Sessions")
            .tag("content")
            .child(&nav)
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
            .width_request(360)
            .height_request(294)
            .content(&split)
            .build();
        window.add_action(&gio::PropertyAction::new(
            "group-by-machine",
            &by_machine,
            "active",
        ));
        let narrow = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
            adw::BreakpointConditionLengthType::MaxWidth,
            600.0,
            adw::LengthUnit::Sp,
        ));
        narrow.add_setter(&split, "collapsed", Some(&true.to_value()));
        window.add_breakpoint(narrow.clone());
        window.set_application(app);

        let this = Self {
            window,
            split,
            sidebar_title,
            sidebar,
            list,
            error,
            content,
            nav,
            sessions_page,
            session_view,
            content_stack,
            groups,
            by_machine,
            state: Rc::default(),
        };
        let selected = this.clone();
        this.list.connect_row_selected(move |_, row| {
            if let Some(row) = row {
                selected.select(row.index());
            }
        });
        // Only the user's pick opens the list on a narrow window; a redraw's does not.
        let shown = this.split.clone();
        this.list
            .connect_row_activated(move |_, _| shown.set_show_content(true));
        let grouped = this.clone();
        this.by_machine.connect_toggled(move |button| {
            grouped.set_grouping(if button.is_active() {
                Grouping::Machines
            } else {
                Grouping::Projects
            });
        });
        let opener = this.clone();
        this.session_view
            .set_opener(Rc::new(move |key| opener.open(&key)));
        let compact = this.clone();
        narrow.connect_apply(move |_| compact.set_compact(true));
        let wide = this.clone();
        narrow.connect_unapply(move |_| wide.set_compact(false));
        this
    }

    pub fn present(&self) {
        self.window.present();
    }

    /// Where the session view's commands go.
    pub fn set_sender(&self, sender: Sender) {
        self.session_view.set_sender(sender);
    }

    /// Opens `key`'s session over the list.
    pub fn open(&self, key: &SessionKey) {
        if !self.show_session(key) {
            return;
        }
        if self.nav.visible_page().as_ref() != Some(self.session_view.page()) {
            if self.nav.find_page("session").is_some() {
                self.nav.pop_to_tag("sessions");
            }
            self.nav.push(self.session_view.page());
        }
        self.split.set_show_content(true);
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
        let session = state.sessions.get(key).unwrap_or(&empty);
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
        self.session_view.show(key, session, machine, recent);
        true
    }

    /// Redraws from the client's machines, keeping the selection, and drops what the
    /// subscriptions said of sessions no longer listed.
    pub fn show_machines(&self, machines: &[Machine]) {
        let listed: HashSet<SessionKey> = lists::keys(machines);
        {
            let mut state = self.state.borrow_mut();
            state.machines = machines.to_vec();
            state.summaries.retain(|key, _| listed.contains(key));
            state.sessions.retain(|key, _| listed.contains(key));
        }
        // The open session follows its machine, or closes when no longer listed.
        if let Some(key) = self.session_view.key()
            && !self.show_session(&key)
        {
            self.session_view.close();
            if self.nav.find_page("session").is_some() {
                self.nav.pop_to_tag("sessions");
            }
        }
        self.sidebar_title.set_subtitle(&summary(machines));
        let scope = self.state.borrow().scope.clone();
        // Rebuilding the rows fires `row-selected` with none; the selection is restored below.
        self.list.remove_all();
        let mut scopes = Vec::new();
        if !machines.is_empty() {
            let row = adw::ActionRow::builder()
                .title("All machines")
                .subtitle(lists::sessions(listed.len()))
                .build();
            row.add_prefix(&gtk::Image::from_icon_name("view-list-symbolic"));
            self.list.append(&row);
            scopes.push(Scope::All);
        }
        for machine in machines {
            let state = lists::connection(&machine.connection);
            let subtitle = format!("{state} · {}", lists::sessions(machine.sessions.len()));
            let mark = mark(&machine.connection);
            self.list
                .append(&sidebar_row(&machine.name, &subtitle, mark, 0));
            scopes.push(Scope::Machine(machine.host_id.clone()));
            for host in &machine.hosts {
                let count = machine
                    .sessions
                    .iter()
                    .filter(|head| head.host_id.as_ref() == Some(&host.host_id))
                    .count();
                let subtitle = format!("{} · {}", lists::host_state(host), lists::sessions(count));
                let mark = if host.online { "success" } else { "error" };
                self.list
                    .append(&sidebar_row(&host.host_name, &subtitle, mark, 1));
                scopes.push(Scope::Host {
                    vault: machine.host_id.clone(),
                    host: host.host_id.clone(),
                });
            }
        }
        self.sidebar
            .set_visible_child_name(if machines.is_empty() { "empty" } else { "list" });
        // A machine or host gone from the list falls back to all machines.
        let index = scopes
            .iter()
            .position(|known| *known == scope)
            .or((!scopes.is_empty()).then_some(0));
        self.state.borrow_mut().scopes = scopes;
        match index.and_then(|index| self.list.row_at_index(i32::try_from(index).ok()?)) {
            Some(row) => self.list.select_row(Some(&row)),
            None => {
                self.state.borrow_mut().scope = Scope::All;
                self.content.set_title("Sessions");
                self.sessions_page.set_title("Sessions");
                self.show_sessions();
            }
        }
    }

    /// Folds in an update of `key`'s session, redrawing the list if it shows the change.
    pub fn apply(&self, key: &SessionKey, update: &SessionUpdate) {
        let changed = {
            let mut state = self.state.borrow_mut();
            // An update can outrun the machine list that drops its session.
            let listed = state.machines.iter().any(|machine| {
                machine.host_id == key.host_id
                    && machine
                        .sessions
                        .iter()
                        .any(|head| head.session_id == key.session_id)
            });
            if listed {
                state.sessions.entry(key.clone()).or_default().apply(update);
            }
            listed
                && state
                    .summaries
                    .entry(key.clone())
                    .or_default()
                    .apply(update)
        };
        if self.session_view.key().as_ref() == Some(key) {
            self.show_session(key);
        }
        if changed {
            self.show_sessions();
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
        let title = {
            let mut state = self.state.borrow_mut();
            let Some(scope) = usize::try_from(index)
                .ok()
                .and_then(|index| state.scopes.get(index))
                .cloned()
            else {
                return;
            };
            let title = scope_title(&state.machines, &scope);
            state.scope = scope;
            title
        };
        self.content.set_title(&title);
        self.sessions_page.set_title(&title);
        self.show_sessions();
    }

    fn set_grouping(&self, grouping: Grouping) {
        self.state.borrow_mut().grouping = grouping;
        self.show_sessions();
    }

    fn set_compact(&self, compact: bool) {
        self.state.borrow_mut().compact = compact;
        self.session_view.set_compact(compact);
        self.show_sessions();
    }

    /// Redraws the session list.
    fn show_sessions(&self) {
        let state = self.state.borrow();
        let lists = Lists {
            machines: &state.machines,
            summaries: &state.summaries,
            compact: state.compact,
        };
        let groups = lists.groups(&state.scope, state.grouping);
        while let Some(child) = self.groups.first_child() {
            self.groups.remove(&child);
        }
        for group in &groups {
            let widget = adw::PreferencesGroup::builder()
                .title(glib::markup_escape_text(&group.title))
                .description(glib::markup_escape_text(&group.description))
                .build();
            for row in &group.rows {
                let widget_row = session_row(row, state.compact);
                let opener = self.clone();
                let key = row.key.clone();
                widget_row.connect_activated(move |_| opener.open(&key));
                widget.add(&widget_row);
            }
            self.groups.append(&widget);
        }
        let any = groups.iter().any(|group| !group.rows.is_empty());
        self.content_stack
            .set_visible_child_name(if any { "sessions" } else { "empty" });
    }
}

#[cfg(test)]
impl MainWindow {
    pub fn set_size(&self, width: i32, height: i32) {
        self.window.set_default_size(width, height);
    }

    pub fn root(&self) -> adw::ApplicationWindow {
        self.window.clone()
    }

    /// The session row titled `title`.
    fn row(&self, title: &str) -> Option<adw::ActionRow> {
        fn find(widget: &gtk::Widget, title: &str) -> Option<adw::ActionRow> {
            let mut child = widget.first_child();
            while let Some(widget) = child {
                if let Some(row) = widget.downcast_ref::<adw::ActionRow>()
                    && row.title() == title
                {
                    return Some(row.clone());
                }
                if let Some(row) = find(&widget, title) {
                    return Some(row);
                }
                child = widget.next_sibling();
            }
            None
        }
        find(self.groups.upcast_ref(), title)
    }

    pub fn has_row(&self, title: &str) -> bool {
        self.row(title).is_some()
    }

    /// Activates the session row titled `title`, as a click does.
    pub fn activate_row(&self, title: &str) {
        self.row(title)
            .expect("the session's row")
            .emit_by_name::<()>("activated", &[]);
    }

    pub fn session_view(&self) -> &SessionView {
        &self.session_view
    }

    pub fn scroll_to_end(&self) {
        self.session_view.scroll_to_end();
    }

    /// Opens the composer's `account`, `model` or `mode` picker.
    pub fn open_picker(&self, name: &str) -> gtk::Popover {
        self.session_view.open_picker(name)
    }
}

/// A sidebar row: a coloured mark, the name and its state; `depth` 1 for a vault's host.
fn sidebar_row(name: &str, subtitle: &str, mark: &str, depth: i32) -> adw::ActionRow {
    let icon = gtk::Image::from_icon_name("media-record-symbolic");
    icon.add_css_class(mark);
    icon.set_margin_start(depth * INDENT);
    // Before the title: the builder may set it first, parsing it as markup.
    let row = adw::ActionRow::new();
    row.set_use_markup(false);
    row.set_title(name);
    row.set_subtitle(subtitle);
    row.add_prefix(&icon);
    row
}

/// A session's row: its status, title and where it runs; its task's child count, how many
/// children need the user, and its PRs.
fn session_row(row: &lists::SessionRow, compact: bool) -> adw::ActionRow {
    let mut subtitle = row.place.clone().unwrap_or_default();
    if let Some(to) = &row.moved_to {
        if !subtitle.is_empty() {
            subtitle.push_str(" · ");
        }
        subtitle.push_str(&format!("moved to {to}"));
    }
    // Before the title: the builder may set it first, parsing it as markup.
    let widget = adw::ActionRow::new();
    widget.set_use_markup(false);
    widget.set_title(&row.title);
    widget.set_subtitle(&subtitle);
    widget.set_activatable(true);
    if row.depth > 0 {
        let indent = i32::try_from(row.depth).unwrap_or(i32::MAX / INDENT);
        widget.add_prefix(
            &gtk::Box::builder()
                .width_request(indent.saturating_mul(INDENT))
                .build(),
        );
    }
    let (label, class) = status(row.status);
    let status = gtk::Label::builder()
        .label(if compact { glyph(row.status) } else { label })
        .tooltip_text(label)
        .xalign(0.0)
        .width_chars(if compact { 1 } else { 9 })
        .build();
    for class in class {
        status.add_css_class(class);
    }
    widget.add_prefix(&status);

    if row.children > 0 {
        let children = gtk::Label::new(Some(&format!("({})", row.children)));
        children.set_tooltip_text(Some(&format!("{} in the task", row.children)));
        children.add_css_class("dim-label");
        widget.add_suffix(&children);
    }
    if row.need_you > 0 {
        let waiting = gtk::Label::new(Some(&format!("!{}", row.need_you)));
        waiting.set_tooltip_text(Some(&format!("{} need you", row.need_you)));
        waiting.add_css_class("accent");
        waiting.add_css_class("heading");
        widget.add_suffix(&waiting);
    }
    let named = if compact { 1 } else { ROW_PRS };
    for pr in row.prs.iter().take(named) {
        widget.add_suffix(&pr_badge(pr));
    }
    if let Some(more) = row.prs.len().checked_sub(named).filter(|more| *more > 0) {
        let more = gtk::Label::new(Some(&format!("+{more}")));
        more.add_css_class("dim-label");
        widget.add_suffix(&more);
    }
    widget
}

/// A PR: its number in its state's colour, then for a live one its checks and a `!` for a
/// conflict or requested changes.
fn pr_badge(pr: &PullRequest) -> gtk::Label {
    let mut text = format!("#{}", pr.number);
    if matches!(pr.state, PrState::Open | PrState::Draft) {
        text.push_str(match pr.ci {
            CiStatus::Passing => " ✓",
            CiStatus::Failing => " ✗",
            CiStatus::Pending => " …",
            CiStatus::None => "",
        });
        if pr.mergeable == Mergeable::Conflicting || pr.review == ReviewStatus::ChangesRequested {
            text.push('!');
        }
    }
    let badge = gtk::Label::new(Some(&text));
    badge.set_tooltip_text(Some(&pr.title));
    badge.add_css_class(match pr.state {
        PrState::Open => "success",
        PrState::Draft => "dim-label",
        PrState::Merged => "accent",
        PrState::Closed => "error",
    });
    badge
}

/// A status's label and style classes.
fn status(status: SessionStatus) -> (&'static str, &'static [&'static str]) {
    match status {
        SessionStatus::Idle => ("idle", &["dim-label"]),
        SessionStatus::Running => ("running", &["warning"]),
        SessionStatus::WaitingForCapacity => ("waiting", &["accent"]),
        SessionStatus::NeedsYou => ("needs you", &["accent", "heading"]),
        SessionStatus::Error => ("error", &["error"]),
        SessionStatus::Archived => ("archived", &["dim-label"]),
        SessionStatus::Moved => ("moved", &["dim-label"]),
        SessionStatus::Unknown => ("?", &["dim-label"]),
    }
}

/// A status as one glyph, for compact rows.
fn glyph(status: SessionStatus) -> &'static str {
    match status {
        SessionStatus::Idle => "·",
        SessionStatus::Running => "●",
        SessionStatus::WaitingForCapacity => "◌",
        SessionStatus::NeedsYou => "!",
        SessionStatus::Error => "✗",
        SessionStatus::Archived => "▪",
        SessionStatus::Moved => "→",
        SessionStatus::Unknown => "?",
    }
}

/// The content page's title for `scope`.
fn scope_title(machines: &[Machine], scope: &Scope) -> String {
    match scope {
        Scope::All => "All machines".to_owned(),
        Scope::Machine(host_id) => machines
            .iter()
            .find(|machine| machine.host_id == *host_id)
            .map(|machine| machine.name.clone())
            .unwrap_or_default(),
        Scope::Host { vault, host } => machines
            .iter()
            .find(|machine| machine.host_id == *vault)
            .and_then(|machine| machine.hosts.iter().find(|h| h.host_id == *host))
            .map(|host| host.host_name.clone())
            .unwrap_or_default(),
    }
}

/// A connection's style class, for its mark.
fn mark(state: &ConnectionState) -> &'static str {
    match state {
        ConnectionState::Connected => "success",
        ConnectionState::Connecting => "warning",
        ConnectionState::Disconnected { .. } => "error",
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

#[cfg(test)]
mod tests {
    use herder_protocol::{EventBody, SessionHead};

    use super::*;
    use crate::lists::tests::{created, fleet, head, key, machine, pr, update};

    /// Every `adw::ActionRow` under `widget`, as title and subtitle.
    fn rows(widget: &gtk::Widget) -> Vec<(String, String)> {
        let mut rows = Vec::new();
        let mut child = widget.first_child();
        while let Some(widget) = child {
            if let Some(row) = widget.downcast_ref::<adw::ActionRow>() {
                rows.push((
                    row.title().into(),
                    row.subtitle().unwrap_or_default().into(),
                ));
            } else {
                rows.extend(self::rows(&widget));
            }
            child = widget.next_sibling();
        }
        rows
    }

    /// The text of every label among `title`'s row's prefixes and suffixes.
    fn badges(window: &MainWindow, title: &str) -> Vec<String> {
        fn find(widget: &gtk::Widget, title: &str) -> Option<adw::ActionRow> {
            let mut child = widget.first_child();
            while let Some(widget) = child {
                if let Some(row) = widget.downcast_ref::<adw::ActionRow>()
                    && row.title() == title
                {
                    return Some(row.clone());
                }
                if let Some(row) = find(&widget, title) {
                    return Some(row);
                }
                child = widget.next_sibling();
            }
            None
        }
        fn labels(widget: &gtk::Widget, out: &mut Vec<String>) {
            let mut child = widget.first_child();
            while let Some(widget) = child {
                match widget.downcast_ref::<gtk::Label>() {
                    Some(label) if label.get_visible() => out.push(label.label().into()),
                    _ => labels(&widget, out),
                }
                child = widget.next_sibling();
            }
        }
        let row = find(window.groups.upcast_ref(), title).expect("the row is shown");
        let subtitle = row.subtitle().unwrap_or_default();
        let mut out = Vec::new();
        labels(row.upcast_ref(), &mut out);
        out.retain(|text| !text.is_empty() && text != title && *text != subtitle);
        out
    }

    fn group_titles(window: &MainWindow) -> Vec<String> {
        let mut titles = Vec::new();
        let mut child = window.groups.first_child();
        while let Some(widget) = child {
            if let Some(group) = widget.downcast_ref::<adw::PreferencesGroup>() {
                titles.push(group.title().into());
            }
            child = widget.next_sibling();
        }
        titles
    }

    fn select(window: &MainWindow, index: i32) {
        let row = window.list.row_at_index(index).expect("a sidebar row");
        window.list.select_row(Some(&row));
    }

    #[test]
    fn summary_counts_connected_machines() {
        let up = machine("h1", "box", Vec::new());
        let mut down = machine("h2", "laptop", Vec::new());
        down.connection = ConnectionState::Disconnected {
            error: "connection refused".to_owned(),
        };
        assert_eq!(summary(&[]), "");
        assert_eq!(summary(std::slice::from_ref(&up)), "all connected");
        assert_eq!(summary(&[up, down]), "1 of 2 connected");
    }

    #[gtk::test]
    fn the_sidebar_lists_machines_and_a_vaults_hosts_with_their_state() {
        adw::init().expect("libadwaita initializes");
        let window = MainWindow::new(None);
        assert_eq!(
            window.sidebar.visible_child_name().as_deref(),
            Some("empty")
        );
        let (mut machines, _) = fleet();
        machines[1].name = "nas <1>".to_owned();
        machines[1].connection = ConnectionState::Disconnected {
            error: "connection refused".to_owned(),
        };
        window.show_machines(&machines);
        assert_eq!(window.sidebar.visible_child_name().as_deref(), Some("list"));
        assert_eq!(window.sidebar_title.subtitle(), "2 of 3 connected");
        assert_eq!(
            rows(window.list.upcast_ref()),
            [
                ("All machines", "7 sessions"),
                ("box", "connected · 4 sessions"),
                ("nas <1>", "connection refused · 1 session"),
                ("vault", "connected · 2 sessions"),
                ("devbox", "online · 1 session"),
                ("laptop", "offline · 2h 5m ago · 1 session"),
            ]
            .map(|(a, b)| (a.to_owned(), b.to_owned()))
        );
        assert_eq!(window.content.title(), "All machines");

        // The selection follows its machine across redraws.
        select(&window, 2);
        assert_eq!(window.content.title(), "nas <1>");
        window.show_machines(&machines[1..]);
        assert_eq!(window.list.selected_row().map(|row| row.index()), Some(1));
        assert_eq!(window.content.title(), "nas <1>");
        // Gone, it falls back to all machines.
        window.show_machines(&machines[2..]);
        assert_eq!(window.list.selected_row().map(|row| row.index()), Some(0));
        assert_eq!(window.content.title(), "All machines");
        window.show_machines(&[]);
        assert_eq!(
            window.sidebar.visible_child_name().as_deref(),
            Some("empty")
        );
        assert_eq!(
            window.content_stack.visible_child_name().as_deref(),
            Some("empty")
        );

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
    fn the_session_list_groups_by_project_or_machine_and_follows_the_selection() {
        adw::init().expect("libadwaita initializes");
        let window = MainWindow::new(None);
        let (machines, summaries) = fleet();
        window.show_machines(&machines);
        assert_eq!(
            window.content_stack.visible_child_name().as_deref(),
            Some("sessions")
        );
        // Until a session's first update, it shows its id and has no project.
        assert_eq!(group_titles(&window), ["App", "web", "No project yet"]);
        for (key, summary) in &summaries {
            let mut events = vec![created(&summary.repo, &summary.branch)];
            if *key == crate::lists::tests::key("h1", "s2") {
                events.push(EventBody::PrLinked {
                    pr: pr(12, PrState::Open, CiStatus::Passing),
                });
            }
            window.apply(key, &update(key.session_id.as_str(), events));
        }
        assert_eq!(group_titles(&window), ["App", "scratch", "web"]);
        assert_eq!(
            rows(window.groups.upcast_ref())[..4],
            [
                ("herder/api", "box"),
                ("write the tests", "box"),
                ("document it", "box"),
                ("herder/fix-login", "nas"),
            ]
            .map(|(a, b)| (a.to_owned(), b.to_owned()))
        );
        assert_eq!(
            badges(&window, "herder/api"),
            ["running", "(2)", "!1", "#12 ✓"]
        );
        assert_eq!(badges(&window, "write the tests"), ["needs you"]);

        window.by_machine.set_active(true);
        assert_eq!(group_titles(&window), ["box", "nas", "devbox", "laptop"]);
        select(&window, 5);
        assert_eq!(group_titles(&window), ["laptop"]);
        assert_eq!(
            rows(window.groups.upcast_ref()),
            [("web · herder/docs".to_owned(), String::new())]
        );
        window.by_machine.set_active(false);
        assert_eq!(group_titles(&window), ["web"]);
        assert_eq!(
            rows(window.groups.upcast_ref()),
            [("herder/docs".to_owned(), "laptop · offline".to_owned())]
        );

        // Narrow, rows show a glyph and the branch's last part.
        select(&window, 1);
        window.set_compact(true);
        assert_eq!(badges(&window, "api"), ["●", "(2)", "!1", "#12 ✓"]);
    }

    #[gtk::test]
    fn the_list_follows_live_changes() {
        adw::init().expect("libadwaita initializes");
        let window = MainWindow::new(None);
        let lone = |status| {
            vec![machine(
                "h1",
                "box",
                vec![SessionHead {
                    status,
                    ..head("s1", Some("github.com/org/app"))
                }],
            )]
        };
        window.show_machines(&lone(SessionStatus::Idle));
        let s1 = key("h1", "s1");
        window.apply(&s1, &update("s1", vec![created("/srv/app", "herder/a")]));
        assert_eq!(badges(&window, "herder/a"), ["idle"]);

        window.show_machines(&lone(SessionStatus::NeedsYou));
        assert_eq!(badges(&window, "herder/a"), ["needs you"]);
        window.apply(
            &s1,
            &update(
                "s1",
                vec![
                    EventBody::BranchCheckedOut {
                        branch: "herder/b".to_owned(),
                    },
                    EventBody::PrLinked {
                        pr: pr(3, PrState::Draft, CiStatus::Failing),
                    },
                ],
            ),
        );
        assert_eq!(badges(&window, "herder/b"), ["needs you", "#3 ✗"]);

        // A session no longer listed is dropped, and an update that outruns its listing too.
        window.show_machines(&[machine("h1", "box", Vec::new())]);
        window.apply(&s1, &update("s1", vec![created("/srv/app", "herder/c")]));
        assert!(window.state.borrow().summaries.is_empty());
        assert_eq!(
            window.content_stack.visible_child_name().as_deref(),
            Some("empty")
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
