//! Navigation, as Herdr does it: the input mode, the `ctrl+x` leader, each row's rolled-up
//! state, the attention list, the open session's tabs, and the frame's layout.
//!
//! - **Modes.** PROMPT while the composer has the keys, NAVIGATE elsewhere, APPROVAL while the
//!   open session asks something, LEADER for the one key after `ctrl+x`. The bottom bar shows
//!   the mode as a badge.
//! - **Leader.** `ctrl+x <key>` does what `<key>` does in NAVIGATE, from any mode, plus `b`
//!   (sidebar) and `d` (details) and the digits (the Nth attention row). It lapses after
//!   [`LEADER_TIMEOUT`].
//! - **State.** A row shows a [`State`]: a session its own; a folded session, a project, a
//!   machine or a host the highest of everything under it ([`crate::ui::state::rollup`]).
//!   *Done* is this client's: a turn that finished while the user looked elsewhere, until
//!   they open the session.
//! - **Attention.** Every session with something going on, flat, the most pressing first.

use std::time::{Duration, Instant};

use herder_protocol::SessionStatus;

use crate::app::{App, Focus, Row};
use crate::session::SessionKey;
use crate::ui::state::{self, State};

/// How long `ctrl+x` waits for its key.
pub const LEADER_TIMEOUT: Duration = Duration::from_secs(2);

/// Sidebar columns, its separator included: the default, and the range a drag may set.
pub const SIDEBAR: u16 = 26;
pub const SIDEBAR_MIN: u16 = 18;
pub const SIDEBAR_MAX: u16 = 36;

/// Columns of the collapsed sidebar's strip of state glyphs, its separator included.
pub const STRIP: u16 = 4;

/// Columns of the details panel, its separator included.
pub const DETAILS: u16 = 42;

/// Screens at least this wide show the details panel beside the main pane.
pub const WIDE: u16 = 120;

/// What keys do now, as the bottom bar's badge says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Keys go to the composer.
    Prompt,
    /// Bare letters are commands.
    Navigate,
    /// The open session asks something: `y`/`n` or a digit answers.
    Approval,
    /// `ctrl+x` waits for its key.
    Leader,
}

impl Mode {
    /// The badge's word.
    pub fn label(self) -> &'static str {
        match self {
            Self::Prompt => "PROMPT",
            Self::Navigate => "NAVIGATE",
            Self::Approval => "APPROVAL",
            Self::Leader => "LEADER",
        }
    }
}

/// A tab of the open session's main pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    /// The transcript and the composer.
    Chat,
    /// The session's task children.
    Tasks,
    /// The session's pull requests.
    Prs,
    /// The session's terminals; owners only. Opens the terminal picker.
    Term,
}

/// The frame's layout, as the user set it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    /// Sidebar columns, its separator included.
    pub sidebar: u16,
    /// The sidebar is a strip of state glyphs.
    pub collapsed: bool,
    /// The details panel is toggled: hidden on a wide screen, shown over the main pane on a
    /// narrower one.
    pub details: bool,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            sidebar: SIDEBAR,
            collapsed: false,
            details: false,
        }
    }
}

/// The NAVIGATE key `ctrl+x <key>` stands for, and what the leader popup calls it.
pub const LEADER_KEYS: &[(char, &str)] = &[
    ('/', "go to"),
    ('n', "new session"),
    ('s', "switch"),
    ('I', "inbox"),
    ('P', "all PRs"),
    ('p', "session PRs"),
    ('A', "accounts"),
    ('m', "fleet"),
    ('t', "terminals"),
    ('v', "group by"),
    ('z', "fold tasks"),
    ('b', "sidebar"),
    ('d', "details"),
    ('r', "reconnect"),
    (':', "commands"),
    ('?', "help"),
    ('q', "quit"),
];

impl App {
    /// The mode the badge shows; `None` while a dialog or the help has the keys.
    pub fn mode(&self) -> Option<Mode> {
        if self.leader.is_some() {
            return Some(Mode::Leader);
        }
        if self.dialog_open() {
            return None;
        }
        Some(match self.focus {
            Focus::Composer => Mode::Prompt,
            Focus::Transcript if self.asking() => Mode::Approval,
            _ => Mode::Navigate,
        })
    }

    /// Whether the open session waits on an approval or a question.
    pub fn asking(&self) -> bool {
        self.open_session()
            .is_some_and(|s| !s.approvals.is_empty() || !s.questions.is_empty())
    }

    /// The open session's tab in view.
    pub fn tab(&self) -> Tab {
        match self.focus {
            Focus::Tasks => Tab::Tasks,
            Focus::Prs => Tab::Prs,
            _ => Tab::Chat,
        }
    }

    /// Shows the open session's `tab`.
    pub(crate) fn show_tab(&mut self, tab: Tab) {
        if self.open.is_none() {
            return;
        }
        match tab {
            Tab::Chat => self.focus = Focus::Transcript,
            Tab::Tasks => {
                self.task_cursor = 0;
                self.focus = Focus::Tasks;
            }
            Tab::Prs => {
                self.prs.strip = 0;
                self.focus = Focus::Prs;
            }
            Tab::Term => {
                let open = self.open.clone();
                self.chosen = self
                    .rows()
                    .into_iter()
                    .find(|row| row.session() == open.as_ref());
                self.open_picker();
            }
        }
    }

    /// The open session's task children, oldest first.
    pub fn tasks(&self) -> Vec<SessionKey> {
        let Some(key) = &self.open else {
            return Vec::new();
        };
        let mut tasks: Vec<SessionKey> = self.children(key).into_iter().cloned().collect();
        tasks.sort_by(|a, b| a.session_id.cmp(&b.session_id));
        tasks
    }

    /// `key`'s own state: needs you while a request waits on the user, done while a finished
    /// turn is unseen.
    pub fn state(&self, key: &SessionKey) -> State {
        let Some(session) = self.sessions.get(key) else {
            return State::Unknown;
        };
        if !session.loaded {
            return State::Unknown;
        }
        if session.needs_user() {
            return State::NeedsYou;
        }
        State::of(session.status, self.done.contains(key))
    }

    /// `key` and every listed task under it.
    pub fn subtree(&self, key: &SessionKey) -> Vec<SessionKey> {
        let mut keys = vec![key.clone()];
        let mut at = 0;
        while let Some(key) = keys.get(at).cloned() {
            for child in self.children(&key) {
                if !keys.contains(child) {
                    keys.push(child.clone());
                }
            }
            at += 1;
        }
        keys
    }

    /// The sessions a sidebar row stands for: a project's, a machine's or a host's; a
    /// session's subtree.
    pub fn row_sessions(&self, row: &Row) -> Vec<SessionKey> {
        let listed = self.machines.iter().flat_map(|machine| {
            machine.sessions.iter().map(move |head| {
                let key = SessionKey {
                    host_id: machine.host_id.clone(),
                    session_id: head.session_id.clone(),
                };
                (key, head.host_id.as_ref())
            })
        });
        match row {
            Row::Session { key, .. } => self.subtree(key),
            Row::Project(project) => listed
                .filter(|(key, _)| self.project_of(key).as_ref() == project.as_ref())
                .map(|(key, _)| key)
                .collect(),
            Row::Machine(host_id) => listed
                .filter(|(key, _)| key.host_id == *host_id)
                .map(|(key, _)| key)
                .collect(),
            Row::Host { vault, host } => listed
                .filter(|(key, on)| key.host_id == *vault && *on == Some(host))
                .map(|(key, _)| key)
                .collect(),
        }
    }

    /// The state a sidebar row shows: a session its own, unless folded; anything else the
    /// highest of its sessions.
    pub fn row_state(&self, row: &Row) -> State {
        match row {
            Row::Session { key, .. } if !self.folded.contains(key) => self.state(key),
            _ => state::rollup(self.row_sessions(row).iter().map(|key| self.state(key))),
        }
    }

    /// How many sessions under a sidebar row need the user: a session's tasks, a project's,
    /// machine's or host's sessions.
    pub fn row_needing(&self, row: &Row) -> usize {
        let keys = self.row_sessions(row);
        let below = match row {
            Row::Session { .. } => &keys[1..],
            _ => &keys[..],
        };
        below
            .iter()
            .filter(|key| self.state(key) == State::NeedsYou)
            .count()
    }

    /// Every session with something going on, the most pressing first, then in sidebar
    /// order.
    pub fn attention(&self) -> Vec<SessionKey> {
        let mut keys: Vec<(State, SessionKey)> = self
            .all_rows()
            .into_iter()
            .filter_map(|row| match row {
                Row::Session { key, .. } => Some((self.state(&key), key)),
                _ => None,
            })
            .filter(|(state, _)| state.priority() >= State::Waiting.priority())
            .collect();
        keys.sort_by_key(|(state, _)| std::cmp::Reverse(state.priority()));
        keys.into_iter().map(|(_, key)| key).collect()
    }

    /// How many sessions are in each state worth a word, the most pressing first: the phone
    /// header's summary.
    pub fn summary(&self) -> Vec<(State, usize)> {
        let states: Vec<State> = self.sessions.keys().map(|key| self.state(key)).collect();
        [
            State::NeedsYou,
            State::Error,
            State::Running,
            State::Done,
            State::Waiting,
        ]
        .into_iter()
        .map(|wanted| (wanted, states.iter().filter(|s| **s == wanted).count()))
        .filter(|(_, n)| *n > 0)
        .collect()
    }

    /// Where the open session is among the listed sessions, from 1, and how many there are.
    pub fn position(&self) -> Option<(usize, usize)> {
        let open = self.open.as_ref()?;
        let sessions: Vec<Row> = self
            .all_rows()
            .into_iter()
            .filter(|row| row.session().is_some())
            .collect();
        let at = sessions
            .iter()
            .position(|row| row.session() == Some(open))?;
        Some((at + 1, sessions.len()))
    }

    /// Notes what an update did to `key`'s status: a turn that ends while the user looks
    /// elsewhere leaves the session *done*. `was_loaded` is false for the history a new
    /// subscription replays, which is no news.
    pub(crate) fn observe(&mut self, key: &SessionKey, before: SessionStatus, was_loaded: bool) {
        let Some(after) = self.sessions.get(key).map(|s| s.status) else {
            return;
        };
        if after != SessionStatus::Idle {
            self.done.remove(key);
            return;
        }
        if was_loaded && before == SessionStatus::Running && self.open.as_ref() != Some(key) {
            self.done.insert(key.clone());
        }
    }

    /// Arms the leader: the next key, within [`LEADER_TIMEOUT`], is a NAVIGATE key.
    pub(crate) fn arm_leader(&mut self, now: Instant) {
        self.leader = Some(now);
    }

    /// Lets the leader lapse once it has waited [`LEADER_TIMEOUT`].
    pub(crate) fn tick(&mut self, now: Instant) {
        if self
            .leader
            .is_some_and(|armed| now.duration_since(armed) >= LEADER_TIMEOUT)
        {
            self.leader = None;
        }
    }

    /// How long until the leader lapses, if it is armed.
    pub fn leader_left(&self, now: Instant) -> Option<Duration> {
        self.leader
            .map(|armed| LEADER_TIMEOUT.saturating_sub(now.duration_since(armed)))
    }

    /// Opens the session of the `at`th attention row, from 0.
    pub(crate) fn open_attention(&mut self, at: usize) {
        let Some(key) = self.attention().into_iter().nth(at) else {
            return;
        };
        self.open_key(key);
    }

    /// Selects `key`'s row, unfolding its task, and opens it.
    pub(crate) fn open_key(&mut self, key: SessionKey) {
        self.reveal(&key);
        self.chosen = self
            .rows()
            .into_iter()
            .find(|row| row.session() == Some(&key));
        // Open from the sidebar's selection, wherever the focus was.
        self.focus = Focus::Sessions;
        self.act(crate::action::Action::Open);
    }

    /// Selects the row above the selected one in the tree: a task's primary, a session's
    /// project, machine or host.
    pub(crate) fn select_parent(&mut self) {
        let rows = self.rows();
        let Some(at) = self.selected_index(&rows) else {
            return;
        };
        let depth = |row: &Row| match row {
            Row::Session { depth, .. } => depth + 2,
            Row::Host { .. } => 1,
            Row::Machine(_) | Row::Project(_) => 0,
        };
        let own = depth(&rows[at]);
        if let Some(parent) = rows[..at].iter().rev().find(|row| depth(row) < own) {
            self.chosen = Some(parent.clone());
        }
    }

    /// Toggles the sidebar between its full width and a strip of glyphs.
    pub(crate) fn toggle_sidebar(&mut self) {
        self.layout.collapsed = !self.layout.collapsed;
    }

    /// Sets the sidebar's width from a drag that reached column `x`.
    pub(crate) fn resize_sidebar(&mut self, x: u16) {
        self.layout.collapsed = false;
        self.layout.sidebar = (x + 1).clamp(SIDEBAR_MIN, SIDEBAR_MAX);
    }
}

#[cfg(test)]
mod tests {
    use herder_protocol::HostId;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;
    use crate::app::{Effect, Msg};
    use crate::fake::{self, key, status, update};

    fn press(app: &mut App, code: KeyCode) -> Vec<Effect> {
        app.update(Msg::Key(KeyEvent::new(code, KeyModifiers::NONE)))
    }

    fn ctrl_x(app: &mut App) {
        app.update(Msg::Key(KeyEvent::new(
            KeyCode::Char('x'),
            KeyModifiers::CONTROL,
        )));
    }

    fn set_status(app: &mut App, session: &str, seq: u64, to: SessionStatus) {
        fake::feed(
            app,
            "h1",
            session,
            update(session, seq, vec![status(to)], vec![]),
        );
    }

    #[test]
    fn a_folded_session_project_and_machine_roll_up_their_subtree() {
        let mut app = fake::tree();
        let s2 = Row::Session {
            key: key("h1", "s2"),
            depth: 0,
        };
        // s2 needs you itself; its tasks run and fail.
        assert_eq!(app.row_state(&s2), State::NeedsYou);
        set_status(&mut app, "s2", 9, SessionStatus::Idle);
        assert_eq!(app.row_state(&s2), State::Idle, "unfolded: its own");
        app.folded.insert(key("h1", "s2"));
        assert_eq!(
            app.row_state(&s2),
            State::Error,
            "folded: error beats running"
        );
        let machine = Row::Machine(HostId::new("h1"));
        assert_eq!(app.row_state(&machine), State::Error);
        assert_eq!(app.row_needing(&machine), 0);
        set_status(&mut app, "s3", 9, SessionStatus::NeedsYou);
        assert_eq!(app.row_state(&machine), State::NeedsYou);
        assert_eq!(app.row_needing(&machine), 1);
        assert_eq!(app.row_needing(&s2), 1, "a session counts its tasks");
        let project = Row::Project(app.project_of(&key("h1", "s1")));
        assert_eq!(app.row_state(&project), State::NeedsYou);
    }

    #[test]
    fn a_turn_ending_elsewhere_is_done_until_opened() {
        let mut app = fake::tree();
        // s3 finishes while s2 is open: done.
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.open, Some(key("h1", "s2")));
        set_status(&mut app, "s3", 9, SessionStatus::Idle);
        assert_eq!(app.state(&key("h1", "s3")), State::Done);
        // The open session's own turn ending is seen.
        set_status(&mut app, "s2", 9, SessionStatus::Running);
        set_status(&mut app, "s2", 10, SessionStatus::Idle);
        assert_eq!(app.state(&key("h1", "s2")), State::Idle);
        // Opening s3 marks it seen.
        app.open_key(key("h1", "s3"));
        assert_eq!(app.state(&key("h1", "s3")), State::Idle);
        // History replayed by a new subscription is no news.
        let mut fresh = fake::tree();
        let replay = update(
            "s1",
            1,
            vec![status(SessionStatus::Running), status(SessionStatus::Idle)],
            vec![],
        );
        fresh.sessions.get_mut(&key("h1", "s1")).unwrap().loaded = false;
        fake::feed(&mut fresh, "h1", "s1", replay);
        assert_eq!(fresh.state(&key("h1", "s1")), State::Idle);
    }

    #[test]
    fn attention_lists_the_most_pressing_first_and_leaves_idle_out() {
        let mut app = fake::tree();
        set_status(&mut app, "s1", 9, SessionStatus::Running);
        let order: Vec<State> = app.attention().iter().map(|k| app.state(k)).collect();
        assert_eq!(
            order,
            [
                State::NeedsYou,
                State::Error,
                State::Running,
                State::Running
            ]
        );
        assert_eq!(
            app.summary(),
            [(State::NeedsYou, 1), (State::Error, 1), (State::Running, 2)]
        );
    }

    #[test]
    fn opening_a_session_lands_in_the_prompt_unless_it_asks_something() {
        let mut app = fake::tree();
        // s1 is idle: PROMPT.
        press(&mut app, KeyCode::Char('G'));
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.mode(), Some(Mode::Prompt));
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.mode(), Some(Mode::Navigate));
        assert_eq!(app.focus, Focus::Transcript);
        press(&mut app, KeyCode::Char('i'));
        assert_eq!(app.mode(), Some(Mode::Prompt));
        // s2 asks for an approval: APPROVAL.
        fake::feed(
            &mut app,
            "h1",
            "s2",
            update(
                "s2",
                3,
                vec![fake::approval("a1", "rm -rf target/")],
                vec![],
            ),
        );
        app.open_key(key("h1", "s2"));
        assert_eq!(app.mode(), Some(Mode::Approval));
        // Esc leaves it pending, for the sidebar.
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.focus, Focus::Sessions);
        assert_eq!(app.mode(), Some(Mode::Navigate));
    }

    #[test]
    fn the_leader_runs_one_navigate_key_from_the_prompt_and_lapses() {
        let mut app = fake::tree();
        press(&mut app, KeyCode::Char('G'));
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.focus, Focus::Composer);
        ctrl_x(&mut app);
        assert_eq!(app.mode(), Some(Mode::Leader));
        // `I` is the inbox, not a letter for the prompt.
        press(&mut app, KeyCode::Char('I'));
        assert_eq!(app.focus, Focus::Inbox);
        assert!(app.compose.editor.is_empty());
        assert!(app.leader.is_none());

        // Leader-only keys: b collapses the sidebar, d toggles the details.
        ctrl_x(&mut app);
        press(&mut app, KeyCode::Char('b'));
        assert!(app.layout.collapsed);
        ctrl_x(&mut app);
        press(&mut app, KeyCode::Char('d'));
        assert!(app.layout.details);

        // A digit opens that attention row: 1 is s2, which needs you.
        ctrl_x(&mut app);
        press(&mut app, KeyCode::Char('1'));
        assert_eq!(app.open, Some(key("h1", "s2")));

        // Esc cancels; an unknown key does nothing and disarms.
        ctrl_x(&mut app);
        assert_eq!(press(&mut app, KeyCode::Esc), []);
        assert!(app.leader.is_none());
        assert_eq!(app.open, Some(key("h1", "s2")));

        // Left alone, it lapses after two seconds.
        ctrl_x(&mut app);
        let armed = app.leader.unwrap();
        app.tick(armed + Duration::from_millis(1500));
        assert!(app.leader.is_some());
        app.tick(armed + LEADER_TIMEOUT);
        assert!(app.leader.is_none());
        assert_eq!(app.mode(), Some(Mode::Prompt));
    }

    #[test]
    fn tabs_show_tasks_and_prs_and_tasks_open_their_session() {
        let mut app = fake::with_prs();
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.tab(), Tab::Chat);
        app.show_tab(Tab::Tasks);
        assert_eq!(app.tab(), Tab::Tasks);
        assert_eq!(app.tasks(), [key("h1", "s3"), key("h1", "s4")]);
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.task_cursor, 1);
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.tab(), Tab::Chat);
        app.show_tab(Tab::Tasks);
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.open, Some(key("h1", "s4")));
        app.open_key(key("h1", "s2"));
        app.show_tab(Tab::Prs);
        assert_eq!(app.focus, Focus::Prs);
        // The terminal tab opens the picker, for an owner.
        app.show_tab(Tab::Term);
        assert!(app.terminals.is_some());
    }

    #[test]
    fn the_sidebar_moves_up_a_level_and_goes_back_to_the_open_session() {
        let mut app = fake::tree();
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(
            app.selected().and_then(|row| row.session().cloned()),
            Some(key("h1", "s3"))
        );
        press(&mut app, KeyCode::Char('h'));
        assert_eq!(
            app.selected().and_then(|row| row.session().cloned()),
            Some(key("h1", "s2"))
        );
        press(&mut app, KeyCode::Left);
        assert_eq!(app.selected(), Some(Row::Machine(HostId::new("h1"))));
        // Esc in the sidebar returns to the open session.
        app.open_key(key("h1", "s1"));
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.focus, Focus::Sessions);
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.focus, Focus::Transcript);
        // `/` goes to the sidebar from anywhere in NAVIGATE.
        press(&mut app, KeyCode::Char('/'));
        assert_eq!(app.focus, Focus::Sessions);
    }

    #[test]
    fn the_sidebar_resizes_within_bounds() {
        let mut app = fake::tree();
        app.resize_sidebar(5);
        assert_eq!(app.layout.sidebar, SIDEBAR_MIN);
        app.resize_sidebar(29);
        assert_eq!(app.layout.sidebar, 30);
        app.resize_sidebar(80);
        assert_eq!(app.layout.sidebar, SIDEBAR_MAX);
    }
}
