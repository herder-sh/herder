//! The app state and its reducer: every input is a [`Msg`], [`App::update`] folds it in and
//! returns the [`Effect`]s the event loop must carry out. Nothing here does I/O, so tests drive
//! it with plain values.

use std::collections::{HashMap, HashSet};

use herder_client_core::{Machine, SessionUpdate};
use herder_protocol::{CommandBody, CommandResult, HostId, ProjectId};
use ratatui::crossterm::event::{KeyEvent, MouseEvent};
use ratatui::widgets::ListState;

use crate::account_screen::AccountScreen;
use crate::action::{self, Action};
use crate::compose::{Compose, Origin};
use crate::glyphs::Glyphs;
use crate::inbox::Inbox;
use crate::machines::MachinePanel;
use crate::mouse::{Click, Hits};
use crate::projects::Grouping;
use crate::prs::Prs;
use crate::recover::Recover;
use crate::session::{Session, SessionKey};
use crate::switch::Switch;
use crate::terminal::{self, Picker};

/// An input to the app.
#[derive(Clone, Debug)]
pub enum Msg {
    /// A key was pressed.
    Key(KeyEvent),
    /// A click, a release or a wheel step: a tap or a swipe on a phone.
    Mouse(MouseEvent),
    /// The terminal changed size: the screen is repainted from scratch.
    Resize,
    /// The terminal got focus back, as a phone app brought to the front: the screen is
    /// repainted from scratch.
    Focus,
    /// The paired machines, as [`herder_client_core::Client::machines`] now lists them.
    Machines(Vec<Machine>),
    /// What changed in a subscribed session.
    Session {
        /// The session.
        key: SessionKey,
        /// The change.
        update: SessionUpdate,
    },
    /// Text was pasted.
    Paste(String),
    /// The daemon answered a command sent for `origin`; `Err` carries why it failed.
    Sent {
        /// What the command was sent for.
        origin: Origin,
        /// The daemon's answer.
        result: Result<CommandResult, String>,
    },
    /// Something to tell the user on the status line, such as a refused command.
    Notice(String),
    /// Pairing a machine ended: the machine, or why it failed.
    Paired(Result<Box<Machine>, String>),
    /// An attached terminal gave the screen back.
    TerminalEnded(terminal::Ended),
}

/// Something the event loop does for the app.
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    /// Leave the TUI.
    Quit,
    /// Reconnect every disconnected machine now.
    Wake,
    /// Send a command to a machine; its answer comes back as [`Msg::Sent`].
    Send {
        /// The machine.
        host_id: HostId,
        /// The command.
        command: CommandBody,
        /// What it is sent for.
        origin: Origin,
    },
    /// Open a web page in the browser.
    OpenUrl(String),
    /// Pair with the daemon of a `herder://pair` link, answering with [`Msg::Paired`].
    Pair(String),
    /// Show a machine by another name on this device; a failure comes back as a notice.
    RenameMachine {
        /// The machine.
        host_id: HostId,
        /// Its new name.
        name: String,
    },
    /// Unpair a machine on this device; a failure comes back as a notice.
    ForgetMachine(HostId),
    /// Clear the screen and draw every cell anew, as after a resize.
    Repaint,
    /// Suspend the TUI and attach the local terminal to a daemon terminal.
    AttachTerminal {
        /// The machine.
        host_id: HostId,
        /// What to attach to.
        target: terminal::Target,
    },
    /// Turn the terminal's mouse reporting on or off.
    Mouse(bool),
    /// Save the app's [`crate::settings::Settings`] in the client profile.
    Save,
}

/// Which pane keys go to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    /// The machines and sessions list.
    Sessions,
    /// The open session's transcript.
    Transcript,
    /// The open session's composer.
    Composer,
    /// The open session's pull request strip.
    Prs,
    /// Every session's pull requests, in the main pane.
    AllPrs,
    /// Everything waiting on the user, in the main pane.
    Inbox,
}

/// A row of the session list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Row {
    /// A machine's heading.
    Machine(HostId),
    /// Under a vault's heading, a host whose sessions the vault lists.
    Host {
        /// The vault.
        vault: HostId,
        /// The host.
        host: HostId,
    },
    /// A project's heading; `None` heads the sessions whose project is not known yet.
    Project(Option<ProjectId>),
    /// A session, `depth` levels into its task tree (0 for a top-level session).
    Session {
        /// The session.
        key: SessionKey,
        /// Nesting under its parent.
        depth: usize,
    },
}

impl Row {
    pub(crate) fn session(&self) -> Option<&SessionKey> {
        match self {
            Row::Machine(_) | Row::Host { .. } | Row::Project(_) => None,
            Row::Session { key, .. } => Some(key),
        }
    }

    fn same(&self, other: &Row) -> bool {
        match (self, other) {
            (Row::Machine(a), Row::Machine(b)) => a == b,
            (Row::Host { vault: a, host: x }, Row::Host { vault: b, host: y }) => a == b && x == y,
            (Row::Project(a), Row::Project(b)) => a == b,
            (Row::Session { key: a, .. }, Row::Session { key: b, .. }) => a == b,
            _ => false,
        }
    }
}

/// Where the transcript is scrolled.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Scroll {
    /// First line shown; `None` follows the end as lines arrive.
    pub top: Option<usize>,
    /// Lines in the transcript at the last draw.
    pub total: usize,
    /// Lines that fit in the pane at the last draw.
    pub height: usize,
}

impl Scroll {
    /// The first line to show now.
    pub fn first_line(&self) -> usize {
        let last_page = self.total.saturating_sub(self.height);
        self.top.map_or(last_page, |top| top.min(last_page))
    }

    pub(crate) fn by(&mut self, lines: isize) {
        let last_page = self.total.saturating_sub(self.height);
        let top = self.first_line().saturating_add_signed(lines);
        // Scrolling to the end follows it again.
        self.top = (top < last_page).then_some(top);
    }

    fn page(&self) -> isize {
        isize::try_from(self.height.max(2) - 1).unwrap_or(isize::MAX)
    }
}

/// Everything the TUI shows.
#[derive(Debug)]
pub struct App {
    /// Paired machines, in pairing order.
    pub machines: Vec<Machine>,
    /// Every listed session of every machine.
    pub sessions: HashMap<SessionKey, Session>,
    /// The row the user selected; until they move, [`App::selected`] is the first session.
    pub(crate) chosen: Option<Row>,
    /// The session the main pane shows.
    pub open: Option<SessionKey>,
    /// Which pane keys go to.
    pub focus: Focus,
    /// Whether the key help is shown.
    pub help: bool,
    /// The first help row shown, when the help does not fit.
    pub help_scroll: usize,
    /// Where the open transcript is scrolled.
    pub scroll: Scroll,
    /// The session list's scroll position, kept between draws.
    pub list: ListState,
    /// The composer, palette and new-session dialog.
    pub compose: Compose,
    /// The pull request strip, view and link prompt.
    pub prs: Prs,
    /// A message for the status line, until the next key.
    pub notice: Option<String>,
    /// The machines panel, if shown.
    pub machine_panel: Option<MachinePanel>,
    /// The terminal picker, while it is open.
    pub terminals: Option<Picker>,
    /// Primary sessions whose children the session list hides.
    pub folded: HashSet<SessionKey>,
    /// The inbox's selection and answer editor.
    pub inbox: Inbox,
    /// The accounts screen, if shown.
    pub account_screen: Option<AccountScreen>,
    /// The switch dialog, while it is open.
    pub switch: Option<Switch>,
    /// The recover dialog, while it is open.
    pub recover: Option<Recover>,
    /// How the session list groups sessions.
    pub grouping: Grouping,
    /// Whether taps and the wheel drive the TUI; `:mouse off` hands them to the terminal.
    pub mouse: bool,
    /// The glyph set `:glyphs` chose; `None` picks by the screen's width.
    pub glyphs: Option<Glyphs>,
    /// Where the last frame's tappable and scrollable spots are.
    pub hits: Hits,
    /// What the press of a tap in progress landed on.
    pub(crate) pressed: Option<Click>,
    /// Where the press of a drag in progress landed, and the row it last reached.
    pub(crate) dragged: Option<(u16, u16, u16)>,
    /// What the last frame's action bar buttons do, in order; empty on a wide screen.
    pub(crate) bar: Vec<Click>,
    /// The action bar button Tab moved to, which Enter presses.
    pub bar_focus: Option<usize>,
}

impl Default for App {
    fn default() -> Self {
        Self {
            machines: Vec::new(),
            sessions: HashMap::new(),
            chosen: None,
            open: None,
            focus: Focus::Sessions,
            help: false,
            help_scroll: 0,
            scroll: Scroll::default(),
            list: ListState::default(),
            compose: Compose::default(),
            prs: Prs::default(),
            notice: None,
            machine_panel: None,
            terminals: None,
            folded: HashSet::new(),
            inbox: Inbox::default(),
            account_screen: None,
            switch: None,
            recover: None,
            grouping: Grouping::default(),
            mouse: true,
            glyphs: None,
            hits: Hits::default(),
            pressed: None,
            dragged: None,
            bar: Vec::new(),
            bar_focus: None,
        }
    }
}

impl App {
    /// Folds in one input; returns what the event loop must do.
    pub fn update(&mut self, msg: Msg) -> Vec<Effect> {
        match msg {
            Msg::Key(key) => {
                self.notice = None;
                if let Some(effects) = self.bar_key(key) {
                    return effects;
                }
                match action::for_key(key, self) {
                    Some(action) => self.act(action),
                    None => Vec::new(),
                }
            }
            Msg::Mouse(event) => self.on_mouse(event),
            Msg::TerminalEnded(ended) => {
                self.notice = Some(ended.notice());
                Vec::new()
            }
            Msg::Resize | Msg::Focus => vec![Effect::Repaint],
            Msg::Machines(machines) => {
                self.machines(machines);
                self.open_pending();
                Vec::new()
            }
            Msg::Session { key, update } => {
                if let Some(session) = self.sessions.get_mut(&key) {
                    session.apply(update);
                }
                Vec::new()
            }
            Msg::Paste(text) => {
                if !self.paste_accounts(&text)
                    && !self.paste_pairing(&text)
                    && !self.paste_inbox(&text)
                {
                    self.paste(&text);
                }
                Vec::new()
            }
            Msg::Sent { origin, result } => {
                // A PR command's failure for a session not in view goes to the status line.
                if let (Origin::Session(key), Err(error)) = (&origin, &result)
                    && (self.open.as_ref() != Some(key)
                        || matches!(self.focus, Focus::AllPrs | Focus::Inbox))
                {
                    self.notice = Some(error.clone());
                }
                self.sent(origin, result);
                Vec::new()
            }
            Msg::Notice(text) => {
                self.notice = Some(text);
                Vec::new()
            }
            Msg::Paired(result) => {
                self.paired(result);
                Vec::new()
            }
        }
    }

    /// Carries out one user action.
    pub fn act(&mut self, action: Action) -> Vec<Effect> {
        if let Action::Compose(act) = action {
            return self.compose(act);
        }
        self.compose.quit_armed = false;
        if self.help {
            match action {
                Action::Up => self.help_scroll = self.help_scroll.saturating_sub(1),
                Action::Down => self.help_scroll += 1,
                Action::PageUp => self.help_scroll = self.help_scroll.saturating_sub(10),
                Action::PageDown => self.help_scroll += 10,
                Action::Top => self.help_scroll = 0,
                // The help clamps its scroll to its last page as it draws.
                Action::Bottom => self.help_scroll = usize::MAX,
                _ => {
                    self.help = false;
                    self.help_scroll = 0;
                }
            }
            return Vec::new();
        }
        if self.terminals.is_some() {
            return self.act_in_picker(action);
        }
        match action {
            Action::Compose(_) => {}
            Action::Terminals => self.open_picker(),
            Action::Quit => return vec![Effect::Quit],
            Action::Reconnect => return vec![Effect::Wake],
            Action::Machines(input) => return self.machine_input(input),
            Action::OpenMachines => self.open_machines(false),
            Action::AddMachine => self.open_machines(true),
            Action::ToggleHelp => self.help = !self.help,
            Action::Pr(action) => return self.act_pr(action),
            Action::Inbox(action) => return self.act_inbox(action),
            Action::Fold => self.fold(),
            Action::OpenAccounts => {
                self.account_screen
                    .get_or_insert_with(AccountScreen::default);
            }
            Action::Accounts(input) => return self.account_input(input),
            Action::OpenSwitch => self.open_switch(),
            Action::Switch(input) => return self.switch_input(input),
            Action::Group => self.toggle_grouping(),
            Action::OpenRecover => self.open_recover(),
            Action::Recover(input) => self.recover_input(input),
            Action::Open => {
                let selected = self.selected();
                if let Some(key) = selected.as_ref().and_then(Row::session).cloned() {
                    if self.open.as_ref() != Some(&key) {
                        self.scroll = Scroll::default();
                        self.prs.strip = 0;
                    }
                    // Pin the row, so new sessions listed above it do not move the selection.
                    self.chosen = selected;
                    self.open = Some(key);
                    self.focus = Focus::Transcript;
                }
            }
            Action::SwitchPane => {
                self.focus = match self.focus {
                    Focus::Sessions if self.open.is_some() => Focus::Transcript,
                    _ => Focus::Sessions,
                };
            }
            Action::Back => self.focus = Focus::Sessions,
            Action::Up | Action::Down | Action::PageUp | Action::PageDown => {
                let (step, page) = match action {
                    Action::Up => (-1, false),
                    Action::Down => (1, false),
                    Action::PageUp => (-1, true),
                    _ => (1, true),
                };
                match self.focus {
                    Focus::Sessions => self.select_by(if page { step * 10 } else { step }),
                    Focus::Transcript | Focus::Composer => {
                        let lines = if page {
                            step * self.scroll.page()
                        } else {
                            step
                        };
                        self.scroll.by(lines);
                    }
                    // Their keys are taken by `prs::for_key` and `inbox::for_key` first.
                    Focus::Prs | Focus::AllPrs | Focus::Inbox => {}
                }
            }
            Action::Top => match self.focus {
                Focus::Sessions => self.chosen = self.rows().into_iter().next(),
                Focus::Transcript | Focus::Composer => self.scroll.top = Some(0),
                Focus::Prs | Focus::AllPrs | Focus::Inbox => {}
            },
            Action::Bottom => match self.focus {
                Focus::Sessions => self.chosen = self.rows().pop(),
                Focus::Transcript | Focus::Composer => self.scroll.top = None,
                Focus::Prs | Focus::AllPrs | Focus::Inbox => {}
            },
        }
        Vec::new()
    }

    /// Opens the terminal picker for the selected session, or says why it cannot.
    fn open_picker(&mut self) {
        let Some(session) = self.selected().as_ref().and_then(Row::session).cloned() else {
            return;
        };
        match terminal::refusal(&self.machines, &session.host_id) {
            Some(refusal) => self.notice = Some(refusal.to_owned()),
            None => {
                self.terminals = Some(Picker {
                    session,
                    selected: 0,
                })
            }
        }
    }

    /// Carries out an action while the terminal picker is open.
    fn act_in_picker(&mut self, action: Action) -> Vec<Effect> {
        let Some(picker) = self.terminals.as_mut() else {
            return Vec::new();
        };
        let rows = terminal::rows(self.machines.as_slice(), &picker.session);
        let last = rows.len().saturating_sub(1);
        picker.selected = picker.selected.min(last);
        match action {
            Action::Quit => return vec![Effect::Quit],
            Action::Up => picker.selected = picker.selected.saturating_sub(1),
            Action::Down => picker.selected = (picker.selected + 1).min(last),
            Action::Top => picker.selected = 0,
            Action::Bottom => picker.selected = last,
            Action::Open => {
                let host_id = picker.session.host_id.clone();
                let target = rows[picker.selected].clone();
                self.terminals = None;
                return vec![Effect::AttachTerminal { host_id, target }];
            }
            Action::Back | Action::Terminals => self.terminals = None,
            _ => {}
        }
        Vec::new()
    }

    /// The sessions to keep subscribed: every listed one, since the list shows their status
    /// and task tree, which only their events carry.
    pub fn wanted(&self) -> HashSet<SessionKey> {
        self.sessions.keys().cloned().collect()
    }

    /// The session list: each machine, then its sessions, newest first, each followed by its
    /// children, oldest first, unless it is folded. Under a vault, each host its sessions run
    /// on heads them.
    pub fn rows(&self) -> Vec<Row> {
        self.tree(true)
    }

    /// [`App::rows`] with every task unfolded.
    pub fn all_rows(&self) -> Vec<Row> {
        self.tree(false)
    }

    fn tree(&self, fold: bool) -> Vec<Row> {
        if self.grouping == Grouping::Projects {
            return self.project_rows(fold);
        }
        let mut rows = Vec::new();
        for machine in &self.machines {
            rows.push(Row::Machine(machine.host_id.clone()));
            // The daemon lists sessions oldest first.
            let keys = |host: Option<&HostId>| -> Vec<SessionKey> {
                machine
                    .sessions
                    .iter()
                    .filter(|head| host.is_none() || head.host_id.as_ref() == host)
                    .map(|head| SessionKey {
                        host_id: machine.host_id.clone(),
                        session_id: head.session_id.clone(),
                    })
                    .collect()
            };
            if machine.hosts.is_empty() {
                rows.extend(self.forest(&keys(None), fold));
                continue;
            }
            // A vault: each host, then the sessions that run on it.
            for host in &machine.hosts {
                rows.push(Row::Host {
                    vault: machine.host_id.clone(),
                    host: host.host_id.clone(),
                });
                rows.extend(self.forest(&keys(Some(&host.host_id)), fold));
            }
        }
        rows
    }

    /// The session rows of `keys`, given oldest first: newest first, each followed by its
    /// children among `keys`, oldest first, unless it is folded.
    pub(crate) fn forest(&self, keys: &[SessionKey], fold: bool) -> Vec<Row> {
        let listed: HashSet<&SessionKey> = keys.iter().collect();
        let parent = |key: &SessionKey| {
            let parent = SessionKey {
                host_id: key.host_id.clone(),
                session_id: self.sessions.get(key)?.parent.clone()?,
            };
            (listed.contains(&parent) && parent != *key).then_some(parent)
        };
        let mut children: HashMap<SessionKey, Vec<&SessionKey>> = HashMap::new();
        let mut roots = Vec::new();
        for key in keys {
            match parent(key) {
                Some(parent) => children.entry(parent).or_default().push(key),
                None => roots.push(key),
            }
        }
        let mut rows = Vec::new();
        let mut stack: Vec<(&SessionKey, usize)> = roots.into_iter().map(|key| (key, 0)).collect();
        let mut seen = HashSet::new();
        while let Some((key, depth)) = stack.pop() {
            if !seen.insert(key) {
                continue;
            }
            rows.push(Row::Session {
                key: key.clone(),
                depth,
            });
            if fold && self.folded.contains(key) {
                continue;
            }
            if let Some(kids) = children.get(key) {
                stack.extend(kids.iter().rev().map(|kid| (*kid, depth + 1)));
            }
        }
        rows
    }

    /// The listed children of `key`'s session, in no order.
    pub fn children(&self, key: &SessionKey) -> Vec<&SessionKey> {
        self.sessions
            .iter()
            .filter(|(child, session)| {
                child.host_id == key.host_id
                    && *child != key
                    && session.parent.as_ref() == Some(&key.session_id)
            })
            .map(|(child, _)| child)
            .collect()
    }

    /// The listed primary session of `key`'s session, for a child.
    pub fn primary(&self, key: &SessionKey) -> Option<(SessionKey, &Session)> {
        let parent = self.sessions.get(key)?.parent.clone()?;
        let primary = SessionKey {
            host_id: key.host_id.clone(),
            session_id: parent,
        };
        let session = self.sessions.get(&primary)?;
        (primary != *key).then_some((primary, session))
    }

    /// Folds the selected task's children away, or unfolds them. On a child, folds its
    /// primary and selects it.
    fn fold(&mut self) {
        let Some(key) = self.selected().as_ref().and_then(Row::session).cloned() else {
            return;
        };
        if !self.children(&key).is_empty() {
            if !self.folded.remove(&key) {
                self.folded.insert(key);
            }
        } else if let Some((primary, _)) = self.primary(&key) {
            self.folded.insert(primary.clone());
            self.chosen = Some(Row::Session {
                key: primary,
                depth: 0,
            });
        }
    }

    /// Unfolds the task `key`'s session belongs to, so its row is listed.
    pub(crate) fn reveal(&mut self, key: &SessionKey) {
        if let Some((primary, _)) = self.primary(key) {
            self.folded.remove(&primary);
        }
    }

    /// The open session, if it is still listed.
    pub fn open_session(&self) -> Option<&Session> {
        self.open.as_ref().and_then(|key| self.sessions.get(key))
    }

    fn machines(&mut self, machines: Vec<Machine>) {
        let listed: HashSet<SessionKey> = machines
            .iter()
            .flat_map(|machine| {
                machine.sessions.iter().map(|head| SessionKey {
                    host_id: machine.host_id.clone(),
                    session_id: head.session_id.clone(),
                })
            })
            .collect();
        self.sessions.retain(|key, _| listed.contains(key));
        self.folded.retain(|key| listed.contains(key));
        for key in listed {
            let id = key.session_id.clone();
            self.sessions.entry(key).or_insert_with(|| Session::new(id));
        }
        self.machines = machines;
        if self
            .open
            .as_ref()
            .is_some_and(|key| !self.sessions.contains_key(key))
        {
            self.open = None;
            self.focus = Focus::Sessions;
        }
        let rows = self.rows();
        if let Some(chosen) = &self.chosen
            && !rows.iter().any(|row| row.same(chosen))
        {
            self.chosen = None;
        }
    }

    /// Selects `row` of the session list.
    pub fn choose_row(&mut self, row: Row) {
        self.chosen = Some(row);
    }

    pub(crate) fn select_by(&mut self, step: isize) {
        let rows = self.rows();
        let Some(last) = rows.len().checked_sub(1) else {
            return;
        };
        let index = match self.selected_index(&rows) {
            Some(at) => at.saturating_add_signed(step).min(last),
            None => 0,
        };
        self.chosen = rows.into_iter().nth(index);
    }

    /// The selected row of the session list.
    pub fn selected(&self) -> Option<Row> {
        let rows = self.rows();
        self.selected_index(&rows).map(|index| rows[index].clone())
    }

    /// Index of the selected row in `rows`, which are [`App::rows`]: the chosen row, else the
    /// first session, else the first machine.
    pub fn selected_index(&self, rows: &[Row]) -> Option<usize> {
        match &self.chosen {
            Some(chosen) => rows.iter().position(|row| row.same(chosen)),
            None => rows
                .iter()
                .position(|row| row.session().is_some())
                .or((!rows.is_empty()).then_some(0)),
        }
    }
}

#[cfg(test)]
mod tests {
    use herder_protocol::SessionStatus;
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};

    use super::*;
    use crate::fake::{self, key, machine};

    fn press(app: &mut App, code: KeyCode) -> Vec<Effect> {
        app.update(Msg::Key(KeyEvent::new(code, KeyModifiers::NONE)))
    }

    fn session(host: &str, id: &str, depth: usize) -> Row {
        Row::Session {
            key: key(host, id),
            depth,
        }
    }

    #[test]
    fn rows_put_children_under_their_parent_and_newest_sessions_first() {
        let app = fake::tree();
        assert_eq!(
            app.rows(),
            [
                Row::Machine(HostId::new("h1")),
                session("h1", "s2", 0),
                session("h1", "s3", 1),
                session("h1", "s4", 1),
                session("h1", "s1", 0),
            ]
        );
    }

    #[test]
    fn a_vaults_sessions_come_under_their_host() {
        let host = |host: &str| Row::Host {
            vault: HostId::new("v"),
            host: HostId::new(host),
        };
        assert_eq!(
            fake::vault().rows(),
            [
                Row::Machine(HostId::new("v")),
                host("devbox"),
                session("v", "s3", 0),
                session("v", "s1", 0),
                host("laptop"),
                session("v", "s2", 0),
            ]
        );
    }

    #[test]
    fn a_child_whose_parent_is_not_listed_is_top_level() {
        let mut app = App {
            grouping: crate::projects::Grouping::Machines,
            ..App::default()
        };
        app.update(Msg::Machines(vec![machine("h1", "box", &["s3"])]));
        fake::feed(
            &mut app,
            "h1",
            "s3",
            fake::update(
                "s3",
                1,
                vec![fake::created("b", Some("s2"), Some("t"))],
                vec![],
            ),
        );
        assert_eq!(
            app.rows(),
            [Row::Machine(HostId::new("h1")), session("h1", "s3", 0)]
        );
    }

    #[test]
    fn every_listed_session_is_wanted_and_unlisted_ones_are_dropped() {
        let mut app = fake::tree();
        assert_eq!(app.wanted().len(), 4);
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.open, Some(key("h1", "s2")));

        app.update(Msg::Machines(vec![machine("h1", "box", &["s1"])]));
        assert_eq!(app.wanted(), HashSet::from([key("h1", "s1")]));
        assert_eq!(app.open, None);
        assert_eq!(app.focus, Focus::Sessions);
        assert_eq!(app.selected(), Some(session("h1", "s1", 0)));
        // Updates for a dropped session change nothing.
        fake::feed(&mut app, "h1", "s2", fake::update("s2", 9, vec![], vec![]));
        assert!(!app.sessions.contains_key(&key("h1", "s2")));
    }

    #[test]
    fn keys_move_the_selection_and_open_a_session() {
        let mut app = fake::tree();
        // The first session is selected to start with.
        assert_eq!(app.selected(), Some(session("h1", "s2", 0)));
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Down);
        assert_eq!(app.selected(), Some(session("h1", "s4", 1)));
        press(&mut app, KeyCode::Char('G'));
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.selected(), Some(session("h1", "s1", 0)));
        press(&mut app, KeyCode::Char('g'));
        assert_eq!(app.selected(), Some(Row::Machine(HostId::new("h1"))));
        // A machine row opens nothing.
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.open, None);
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.open, Some(key("h1", "s2")));
        assert_eq!(app.focus, Focus::Transcript);

        // In the transcript, j/k scroll instead of moving the selection.
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.selected(), Some(session("h1", "s2", 0)));
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.focus, Focus::Sessions);
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.focus, Focus::Transcript);
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.focus, Focus::Sessions);
    }

    #[test]
    fn an_opened_session_stays_selected_when_a_newer_one_is_listed() {
        let mut app = fake::tree();
        press(&mut app, KeyCode::Enter);
        let listed = machine("h1", "box", &["s1", "s2", "s3", "s4", "s5"]);
        app.update(Msg::Machines(vec![listed]));
        assert_eq!(app.rows()[1], session("h1", "s5", 0));
        assert_eq!(app.selected(), Some(session("h1", "s2", 0)));
    }

    #[test]
    fn the_selection_survives_a_new_machine_list() {
        let mut app = fake::tree();
        press(&mut app, KeyCode::Char('G'));
        let mut machines = app.machines.clone();
        machines.push(machine("h2", "other", &["x1"]));
        app.update(Msg::Machines(machines));
        assert_eq!(app.selected(), Some(session("h1", "s1", 0)));
    }

    #[test]
    fn quit_reconnect_and_help() {
        let mut app = fake::tree();
        assert_eq!(press(&mut app, KeyCode::Char('r')), [Effect::Wake]);
        press(&mut app, KeyCode::Char('?'));
        assert!(app.help);
        // While the help is shown, any key just closes it.
        assert_eq!(press(&mut app, KeyCode::Char('q')), []);
        assert!(!app.help);
        assert_eq!(press(&mut app, KeyCode::Char('q')), [Effect::Quit]);
        // With no turn to interrupt, Ctrl-C quits on its second press.
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(app.update(Msg::Key(ctrl_c)), []);
        assert_eq!(app.update(Msg::Key(ctrl_c)), [Effect::Quit]);
    }

    #[test]
    fn the_transcript_follows_its_end_until_scrolled_up() {
        let mut app = fake::tree();
        press(&mut app, KeyCode::Enter);
        app.scroll.total = 100;
        app.scroll.height = 10;
        assert_eq!(app.scroll.first_line(), 90);
        // More lines arrive while following: the end stays in view.
        app.scroll.total = 120;
        assert_eq!(app.scroll.first_line(), 110);

        press(&mut app, KeyCode::Char('k'));
        assert_eq!(app.scroll.top, Some(109));
        app.scroll.total = 130;
        assert_eq!(app.scroll.first_line(), 109, "scrolled up stays put");
        press(&mut app, KeyCode::PageUp);
        assert_eq!(app.scroll.first_line(), 100);
        press(&mut app, KeyCode::Char('g'));
        assert_eq!(app.scroll.first_line(), 0);
        press(&mut app, KeyCode::Char('k'));
        assert_eq!(app.scroll.first_line(), 0);

        // Scrolling back to the end follows again.
        for _ in 0..20 {
            press(&mut app, KeyCode::PageDown);
        }
        assert_eq!(app.scroll.top, None);
        press(&mut app, KeyCode::Char('k'));
        press(&mut app, KeyCode::Char('G'));
        assert_eq!(app.scroll.top, None);
    }

    #[test]
    fn opening_another_session_starts_at_its_end() {
        let mut app = fake::tree();
        press(&mut app, KeyCode::Enter);
        app.scroll = Scroll {
            top: Some(3),
            total: 50,
            height: 10,
        };
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.open, Some(key("h1", "s3")));
        assert_eq!(app.scroll.top, None);
        assert_eq!(
            app.sessions[&key("h1", "s3")].status,
            SessionStatus::Running
        );
    }

    #[test]
    fn z_folds_a_tasks_children_and_unfolds_them() {
        let mut app = fake::tree();
        assert_eq!(app.selected(), Some(session("h1", "s2", 0)));
        press(&mut app, KeyCode::Char('z'));
        assert_eq!(
            app.rows(),
            [
                Row::Machine(HostId::new("h1")),
                session("h1", "s2", 0),
                session("h1", "s1", 0),
            ]
        );
        // Folded children stay subscribed, so the primary's badge stays current.
        assert_eq!(app.wanted().len(), 4);
        assert_eq!(app.all_rows().len(), 5);
        press(&mut app, KeyCode::Char('z'));
        assert_eq!(app.rows().len(), 5);

        // On a child, z folds its primary and selects it.
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.selected(), Some(session("h1", "s3", 1)));
        press(&mut app, KeyCode::Char('z'));
        assert_eq!(app.rows().len(), 3);
        assert_eq!(app.selected(), Some(session("h1", "s2", 0)));

        // A session with no task does nothing.
        press(&mut app, KeyCode::Char('G'));
        press(&mut app, KeyCode::Char('z'));
        assert_eq!(app.folded, HashSet::from([key("h1", "s2")]));
    }

    #[test]
    fn plain_keys_go_back_from_every_view_a_session_list_opens() {
        let mut app = fake::with_prs();
        for back in [KeyCode::Backspace, KeyCode::Char('h')] {
            press(&mut app, KeyCode::Enter);
            assert_eq!(app.focus, Focus::Transcript);
            press(&mut app, back);
            assert_eq!(app.focus, Focus::Sessions);
        }
        press(&mut app, KeyCode::Char('p'));
        assert_eq!(app.focus, Focus::Prs);
        press(&mut app, KeyCode::Backspace);
        assert_eq!(app.focus, Focus::Transcript);
        press(&mut app, KeyCode::Backspace);
        press(&mut app, KeyCode::Char('P'));
        assert_eq!(app.focus, Focus::AllPrs);
        press(&mut app, KeyCode::Backspace);
        press(&mut app, KeyCode::Char('I'));
        assert_eq!(app.focus, Focus::Inbox);
        press(&mut app, KeyCode::Backspace);
        assert_ne!(app.focus, Focus::Inbox);
        press(&mut app, KeyCode::Char('m'));
        press(&mut app, KeyCode::Backspace);
        assert!(app.machine_panel.is_none());
    }

    #[test]
    fn b_scrolls_the_transcript_up_a_page() {
        let mut app = fake::tree();
        press(&mut app, KeyCode::Enter);
        app.scroll.total = 100;
        app.scroll.height = 10;
        press(&mut app, KeyCode::Char('b'));
        assert_eq!(app.scroll.first_line(), 81);
    }

    #[test]
    fn a_resize_or_focus_asks_for_a_repaint() {
        let mut app = fake::tree();
        assert_eq!(app.update(Msg::Resize), [Effect::Repaint]);
        assert_eq!(app.update(Msg::Focus), [Effect::Repaint]);
    }

    #[test]
    fn a_child_knows_its_primary_and_a_primary_its_children() {
        let app = fake::tree();
        let mut children = app.children(&key("h1", "s2"));
        children.sort_by(|a, b| a.session_id.cmp(&b.session_id));
        assert_eq!(children, [&key("h1", "s3"), &key("h1", "s4")]);
        assert!(app.children(&key("h1", "s1")).is_empty());
        let (primary, _) = app.primary(&key("h1", "s3")).unwrap();
        assert_eq!(primary, key("h1", "s2"));
        assert!(app.primary(&key("h1", "s2")).is_none());
    }
}
