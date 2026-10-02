//! The app state and its reducer: every input is a [`Msg`], [`App::update`] folds it in and
//! returns the [`Effect`]s the event loop must carry out. Nothing here does I/O, so tests drive
//! it with plain values.

use std::collections::{HashMap, HashSet};

use herder_client_core::{Machine, SessionUpdate};
use herder_protocol::{HostId, SessionId};
use ratatui::crossterm::event::KeyEvent;
use ratatui::widgets::ListState;

use crate::action::{self, Action};
use crate::session::{Session, SessionKey};

/// An input to the app.
#[derive(Clone, Debug)]
pub enum Msg {
    /// A key was pressed.
    Key(KeyEvent),
    /// The terminal changed size; only a redraw is needed.
    Resize,
    /// The paired machines, as [`herder_client_core::Client::machines`] now lists them.
    Machines(Vec<Machine>),
    /// What changed in a subscribed session.
    Session {
        /// The session.
        key: SessionKey,
        /// The change.
        update: SessionUpdate,
    },
}

/// Something the event loop does for the app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    /// Leave the TUI.
    Quit,
    /// Reconnect every disconnected machine now.
    Wake,
}

/// Which pane keys go to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    /// The machines and sessions list.
    Sessions,
    /// The open session's transcript.
    Transcript,
}

/// A row of the session list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Row {
    /// A machine's heading.
    Machine(HostId),
    /// A session, `depth` levels into its task tree (0 for a top-level session).
    Session {
        /// The session.
        key: SessionKey,
        /// Nesting under its parent.
        depth: usize,
    },
}

impl Row {
    fn session(&self) -> Option<&SessionKey> {
        match self {
            Row::Machine(_) => None,
            Row::Session { key, .. } => Some(key),
        }
    }

    fn same(&self, other: &Row) -> bool {
        match (self, other) {
            (Row::Machine(a), Row::Machine(b)) => a == b,
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

    fn by(&mut self, lines: isize) {
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
    chosen: Option<Row>,
    /// The session the main pane shows.
    pub open: Option<SessionKey>,
    /// Which pane keys go to.
    pub focus: Focus,
    /// Whether the key help is shown.
    pub help: bool,
    /// Where the open transcript is scrolled.
    pub scroll: Scroll,
    /// The session list's scroll position, kept between draws.
    pub list: ListState,
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
            scroll: Scroll::default(),
            list: ListState::default(),
        }
    }
}

impl App {
    /// Folds in one input; returns what the event loop must do.
    pub fn update(&mut self, msg: Msg) -> Vec<Effect> {
        match msg {
            Msg::Key(key) => match action::for_key(key, self) {
                Some(action) => self.act(action),
                None => Vec::new(),
            },
            Msg::Resize => Vec::new(),
            Msg::Machines(machines) => {
                self.machines(machines);
                Vec::new()
            }
            Msg::Session { key, update } => {
                if let Some(session) = self.sessions.get_mut(&key) {
                    session.apply(update);
                }
                Vec::new()
            }
        }
    }

    /// Carries out one user action.
    pub fn act(&mut self, action: Action) -> Vec<Effect> {
        match action {
            Action::Quit => return vec![Effect::Quit],
            Action::Reconnect => return vec![Effect::Wake],
            Action::ToggleHelp => self.help = !self.help,
            Action::Open => {
                let selected = self.selected();
                if let Some(key) = selected.as_ref().and_then(Row::session).cloned() {
                    if self.open.as_ref() != Some(&key) {
                        self.scroll = Scroll::default();
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
                    Focus::Transcript => {
                        let lines = if page {
                            step * self.scroll.page()
                        } else {
                            step
                        };
                        self.scroll.by(lines);
                    }
                }
            }
            Action::Top => match self.focus {
                Focus::Sessions => self.chosen = self.rows().into_iter().next(),
                Focus::Transcript => self.scroll.top = Some(0),
            },
            Action::Bottom => match self.focus {
                Focus::Sessions => self.chosen = self.rows().pop(),
                Focus::Transcript => self.scroll.top = None,
            },
        }
        Vec::new()
    }

    /// The sessions to keep subscribed: every listed one, since the list shows their status
    /// and task tree, which only their events carry.
    pub fn wanted(&self) -> HashSet<SessionKey> {
        self.sessions.keys().cloned().collect()
    }

    /// The session list: each machine, then its sessions, newest first, each followed by its
    /// children, oldest first.
    pub fn rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        for machine in &self.machines {
            rows.push(Row::Machine(machine.host_id.clone()));
            let listed: HashSet<&SessionId> = machine
                .sessions
                .iter()
                .map(|head| &head.session_id)
                .collect();
            let key = |session_id: &SessionId| SessionKey {
                host_id: machine.host_id.clone(),
                session_id: session_id.clone(),
            };
            let parent = |session_id: &SessionId| {
                self.sessions
                    .get(&key(session_id))
                    .and_then(|session| session.parent.as_ref())
                    .filter(|parent| listed.contains(parent) && *parent != session_id)
            };
            let mut children: HashMap<&SessionId, Vec<&SessionId>> = HashMap::new();
            let mut roots = Vec::new();
            for head in &machine.sessions {
                match parent(&head.session_id) {
                    Some(parent) => children.entry(parent).or_default().push(&head.session_id),
                    None => roots.push(&head.session_id),
                }
            }
            // The daemon lists sessions oldest first.
            let mut stack: Vec<(&SessionId, usize)> = roots.into_iter().map(|id| (id, 0)).collect();
            let mut seen = HashSet::new();
            while let Some((session_id, depth)) = stack.pop() {
                if !seen.insert(session_id) {
                    continue;
                }
                rows.push(Row::Session {
                    key: key(session_id),
                    depth,
                });
                if let Some(kids) = children.get(session_id) {
                    stack.extend(kids.iter().rev().map(|kid| (*kid, depth + 1)));
                }
            }
        }
        rows
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

    fn select_by(&mut self, step: isize) {
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
    fn a_child_whose_parent_is_not_listed_is_top_level() {
        let mut app = App::default();
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
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
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
}
