//! The key maps, as tables: one source for what a key does and for the help that says so.
//!
//! The NAVIGATE keys, the leader's, the terminal picker's and the help's own are [`Bind`]s:
//! [`crate::action`] dispatches by them. The composer, the inbox, the board, the pull
//! requests and the dialogs match their keys in their own modules, against more state than a
//! table holds; their [`Keys`] here are checked against that dispatch by a test that presses
//! every key, so neither can drift from the other.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use herder_protocol::ApprovalDecision;

use crate::action::Action;
use crate::app::Focus;
use crate::board::BoardAction;
use crate::chat::ChatAct;
use crate::compose::Act;
use crate::inbox::InboxAction;
use crate::prompt::COMMANDS;
use crate::prs::PrAction;

/// A key, and the modifiers it needs held; any others held too are ignored, so `k` is `k`
/// with Shift or Alt as well.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Key {
    pub code: KeyCode,
    pub mods: KeyModifiers,
}

impl Key {
    /// Whether `key` presses this key.
    pub fn matches(&self, key: KeyEvent) -> bool {
        key.code == self.code && key.modifiers.contains(self.mods)
    }

    /// The key as the help and the hint bar write it: `ctrl+u`, `enter`, `↑`.
    pub fn name(&self) -> String {
        let mut name = String::new();
        if self.mods.contains(KeyModifiers::CONTROL) {
            name.push_str("ctrl+");
        }
        if self.mods.contains(KeyModifiers::ALT) {
            name.push_str("alt+");
        }
        if self.mods.contains(KeyModifiers::SUPER) {
            name.push_str("cmd+");
        }
        match self.code {
            KeyCode::Char(' ') => name.push_str("space"),
            KeyCode::Char(c) => name.push(c),
            KeyCode::Enter => name.push_str("enter"),
            KeyCode::Esc => name.push_str("esc"),
            KeyCode::Tab => name.push_str("tab"),
            KeyCode::BackTab => name.push_str("shift+tab"),
            KeyCode::Backspace => name.push('⌫'),
            KeyCode::Up => name.push('↑'),
            KeyCode::Down => name.push('↓'),
            KeyCode::Left => name.push('←'),
            KeyCode::Right => name.push('→'),
            KeyCode::PageUp => name.push_str("pgup"),
            KeyCode::PageDown => name.push_str("pgdn"),
            KeyCode::Home => name.push_str("home"),
            KeyCode::End => name.push_str("end"),
            other => name.push_str(&other.to_string().to_lowercase()),
        }
        name
    }
}

const fn code(code: KeyCode) -> Key {
    Key {
        code,
        mods: KeyModifiers::NONE,
    }
}

const fn ch(c: char) -> Key {
    code(KeyCode::Char(c))
}

const fn ctrl(c: char) -> Key {
    Key {
        code: KeyCode::Char(c),
        mods: KeyModifiers::CONTROL,
    }
}

/// `c` with Cmd, which crossterm calls Super.
const fn cmd(c: char) -> Key {
    Key {
        code: KeyCode::Char(c),
        mods: KeyModifiers::SUPER,
    }
}

const fn alt(code: KeyCode) -> Key {
    Key {
        code,
        mods: KeyModifiers::ALT,
    }
}

const ENTER: Key = code(KeyCode::Enter);
const ESC: Key = code(KeyCode::Esc);
const TAB: Key = code(KeyCode::Tab);
const BACKTAB: Key = code(KeyCode::BackTab);
const BACKSPACE: Key = code(KeyCode::Backspace);
const UP: Key = code(KeyCode::Up);
const DOWN: Key = code(KeyCode::Down);
const LEFT: Key = code(KeyCode::Left);
const RIGHT: Key = code(KeyCode::Right);
const PGUP: Key = code(KeyCode::PageUp);
const PGDN: Key = code(KeyCode::PageDown);
const HOME: Key = code(KeyCode::Home);
const END: Key = code(KeyCode::End);

/// `1` to `9`, which the help writes `1-9`.
pub const DIGITS: &[Key] = &[
    ch('1'),
    ch('2'),
    ch('3'),
    ch('4'),
    ch('5'),
    ch('6'),
    ch('7'),
    ch('8'),
    ch('9'),
];

/// Where a [`Bind`] works: what must have focus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum On {
    Anywhere,
    Sessions,
    Transcript,
    /// The session list or the tasks tab.
    Lists,
    /// The transcript or the tasks tab.
    Inside,
    /// The session list or the transcript: what acts on a session.
    Session,
}

impl On {
    pub fn holds(self, focus: Focus) -> bool {
        match self {
            Self::Anywhere => true,
            Self::Sessions => focus == Focus::Sessions,
            Self::Transcript => focus == Focus::Transcript,
            Self::Lists => matches!(focus, Focus::Sessions | Focus::Tasks),
            Self::Inside => matches!(focus, Focus::Transcript | Focus::Tasks),
            Self::Session => matches!(focus, Focus::Sessions | Focus::Transcript),
        }
    }
}

/// Keys, the action they ask for, where, and what the help calls it.
#[derive(Clone, Copy, Debug)]
pub struct Bind {
    pub keys: &'static [Key],
    pub on: On,
    pub action: Action,
    pub does: &'static str,
}

const fn bind(keys: &'static [Key], on: On, action: Action, does: &'static str) -> Bind {
    Bind {
        keys,
        on,
        action,
        does,
    }
}

/// The first of `binds` that `key` presses where `focus` is, by its action.
pub fn lookup(binds: &[Bind], key: KeyEvent, focus: Focus) -> Option<Action> {
    binds
        .iter()
        .find(|bind| bind.on.holds(focus) && bind.keys.iter().any(|k| k.matches(key)))
        .map(|bind| bind.action)
}

use On::{Anywhere, Inside, Lists, Session, Sessions, Transcript};

/// NAVIGATE's keys, the first match winning. Before them, [`crate::action::for_key`] gives
/// the transcript's digits and Enter on an item, and the sidebar's way back to the open
/// session; [`extras`] lists those for the help.
pub const NAVIGATE: &[Bind] = &[
    bind(
        &[ch('[')],
        Transcript,
        Action::Chat(ChatAct::Prev),
        "previous item",
    ),
    bind(
        &[ch(']')],
        Transcript,
        Action::Chat(ChatAct::Next),
        "next item",
    ),
    bind(
        &[ch('e')],
        Transcript,
        Action::Chat(ChatAct::Toggle),
        "expand the item: tool output, diff, reasoning",
    ),
    bind(
        &[ch('c')],
        Transcript,
        Action::Chat(ChatAct::Copy),
        "copy the item",
    ),
    bind(
        &[ch('x')],
        Transcript,
        Action::Chat(ChatAct::Stop),
        "stop the running turn",
    ),
    bind(
        &[ch('o')],
        Transcript,
        Action::Chat(ChatAct::OpenImages),
        "open the item's images, else the last prompt's",
    ),
    bind(
        &[ch('w')],
        Transcript,
        Action::Chat(ChatAct::SaveImages),
        "save the item's images, else the last prompt's",
    ),
    bind(
        &[ch('i'), ENTER],
        Transcript,
        Action::Compose(Act::Write),
        "write in the session",
    ),
    bind(
        &[ch('y')],
        Transcript,
        Action::Compose(Act::Approve(ApprovalDecision::Allow)),
        "allow the pending approval",
    ),
    bind(
        &[ch('n')],
        Transcript,
        Action::Compose(Act::Approve(ApprovalDecision::Deny)),
        "deny the pending approval",
    ),
    bind(
        &[ch('i')],
        Sessions,
        Action::Inbox(InboxAction::Toggle),
        "inbox",
    ),
    bind(
        &[ch(':')],
        Anywhere,
        Action::Compose(Act::Palette),
        "command palette",
    ),
    bind(
        &[ch('/')],
        Anywhere,
        Action::GoTo,
        "go to a session: the sidebar, or the switcher",
    ),
    bind(
        &[ch('n')],
        Anywhere,
        Action::Compose(Act::NewSession),
        "new session",
    ),
    bind(&[ch('q')], Anywhere, Action::Quit, "quit"),
    bind(&[ch('?')], Anywhere, Action::ToggleHelp, "this help"),
    bind(&[ch('k'), UP], Anywhere, Action::Up, "up, or scroll up"),
    bind(
        &[ch('j'), DOWN],
        Anywhere,
        Action::Down,
        "down, or scroll down",
    ),
    bind(
        &[ctrl('u'), PGUP, ch('b')],
        Anywhere,
        Action::PageUp,
        "page up",
    ),
    bind(
        &[ctrl('d'), PGDN, ch(' ')],
        Anywhere,
        Action::PageDown,
        "page down",
    ),
    bind(&[ch('g'), HOME], Anywhere, Action::Top, "first"),
    bind(
        &[ch('G'), END],
        Anywhere,
        Action::Bottom,
        "last; follows the transcript",
    ),
    bind(
        &[ENTER, ch('l'), RIGHT],
        Lists,
        Action::Open,
        "open the selected session",
    ),
    bind(
        &[TAB, BACKTAB],
        Anywhere,
        Action::SwitchPane,
        "switch pane; on a phone, move along the buttons",
    ),
    bind(
        &[ESC, BACKSPACE, ch('h'), LEFT],
        Inside,
        Action::Back,
        "back to the sessions",
    ),
    bind(
        &[BACKSPACE, ch('h'), LEFT],
        Sessions,
        Action::Parent,
        "up a level",
    ),
    bind(
        &[ch('I')],
        Anywhere,
        Action::Inbox(InboxAction::Toggle),
        "inbox",
    ),
    bind(
        &[ch('B')],
        Anywhere,
        Action::Board(BoardAction::Toggle),
        "board: every session's work state",
    ),
    bind(
        &[ch('p')],
        Anywhere,
        Action::Pr(PrAction::FocusStrip),
        "the session's pull requests",
    ),
    bind(
        &[ch('P')],
        Anywhere,
        Action::Pr(PrAction::ToggleAll),
        "every session's pull requests",
    ),
    bind(
        &[ch('L')],
        Anywhere,
        Action::Pr(PrAction::StartLink),
        "link a pull request by number or URL",
    ),
    bind(&[ch('r')], Anywhere, Action::Reconnect, "reconnect now"),
    bind(
        &[ch('m')],
        Anywhere,
        Action::OpenMachines,
        "fleet: machines and fingerprints",
    ),
    bind(
        &[ch('a')],
        Anywhere,
        Action::AddMachine,
        "add a machine, or paste its link",
    ),
    bind(
        &[ch('t')],
        Anywhere,
        Action::Terminals,
        "terminals of the session (owners)",
    ),
    bind(
        &[ch('z')],
        Sessions,
        Action::Fold,
        "fold or unfold the task's children",
    ),
    bind(
        &[ch('A')],
        Anywhere,
        Action::OpenAccounts,
        "accounts: usage and limits",
    ),
    bind(
        &[ch('s')],
        Session,
        Action::OpenSwitch,
        "switch the session's account, provider or model",
    ),
    bind(
        &[ch('v')],
        Anywhere,
        Action::Group,
        "group sessions by project or by machine",
    ),
    bind(
        &[ch('H')],
        Anywhere,
        Action::ToggleArchived,
        "show or hide archived sessions",
    ),
    bind(
        &[ch('F')],
        Session,
        Action::OpenFork,
        "fork the session onto a host",
    ),
    bind(&[ch('E')], Session, Action::Rename, "rename the session"),
    bind(
        &[ch('R')],
        Session,
        Action::Retitle,
        "ask AI to title the session again",
    ),
];

/// What `ctrl+x <key>` does; short, for the leader's popup. Digits, before them, open that
/// row of the attention list.
pub const LEADER: &[Bind] = &[
    bind(&[ch('/')], Anywhere, Action::GoTo, "go to"),
    bind(
        &[ch('n')],
        Anywhere,
        Action::Compose(Act::NewSession),
        "new session",
    ),
    bind(&[ch('s')], Anywhere, Action::OpenSwitch, "switch"),
    bind(
        &[ch('I')],
        Anywhere,
        Action::Inbox(InboxAction::Toggle),
        "inbox",
    ),
    bind(
        &[ch('B')],
        Anywhere,
        Action::Board(BoardAction::Toggle),
        "board",
    ),
    bind(
        &[ch('P')],
        Anywhere,
        Action::Pr(PrAction::ToggleAll),
        "all PRs",
    ),
    bind(
        &[ch('p')],
        Anywhere,
        Action::Pr(PrAction::FocusStrip),
        "session PRs",
    ),
    bind(
        &[ch('L')],
        Anywhere,
        Action::Pr(PrAction::StartLink),
        "link a PR",
    ),
    bind(&[ch('A')], Anywhere, Action::OpenAccounts, "accounts"),
    bind(&[ch('m')], Anywhere, Action::OpenMachines, "fleet"),
    bind(&[ch('a')], Anywhere, Action::AddMachine, "add machine"),
    bind(&[ch('t')], Anywhere, Action::Terminals, "terminals"),
    bind(&[ch('F')], Anywhere, Action::OpenFork, "fork"),
    bind(&[ch('E')], Anywhere, Action::Rename, "rename"),
    bind(&[ch('R')], Anywhere, Action::Retitle, "retitle"),
    bind(&[ch('v')], Anywhere, Action::Group, "group by"),
    bind(&[ch('H')], Anywhere, Action::ToggleArchived, "archived"),
    bind(&[ch('z')], Anywhere, Action::Fold, "fold tasks"),
    bind(&[ch('b')], Anywhere, Action::ToggleSidebar, "sidebar"),
    bind(&[ch('d')], Anywhere, Action::ToggleDetails, "details"),
    bind(&[ch('r')], Anywhere, Action::Reconnect, "reconnect"),
    bind(
        &[ch(':')],
        Anywhere,
        Action::Compose(Act::Palette),
        "commands",
    ),
    bind(&[ch('?')], Anywhere, Action::ToggleHelp, "help"),
    bind(&[ch('q')], Anywhere, Action::Quit, "quit"),
];

/// The terminal picker's keys.
pub const TERMINALS: &[Bind] = &[
    bind(&[ch('k'), UP], Anywhere, Action::Up, "up"),
    bind(&[ch('j'), DOWN], Anywhere, Action::Down, "down"),
    bind(&[ch('g'), HOME, PGUP], Anywhere, Action::Top, "first"),
    bind(&[ch('G'), END, PGDN], Anywhere, Action::Bottom, "last"),
    bind(&[ENTER], Anywhere, Action::Open, "open or attach"),
    bind(
        &[ESC, BACKSPACE, ch('q'), ch('t')],
        Anywhere,
        Action::Back,
        "close",
    ),
];

/// The help's own keys; the rest do nothing while it shows.
pub const HELP: &[Bind] = &[
    bind(&[ch('k'), UP], Anywhere, Action::Up, "scroll up"),
    bind(&[ch('j'), DOWN], Anywhere, Action::Down, "scroll down"),
    bind(&[PGUP], Anywhere, Action::PageUp, "page up"),
    bind(&[ch(' '), PGDN], Anywhere, Action::PageDown, "page down"),
    bind(&[HOME], Anywhere, Action::Top, "top"),
    bind(&[END], Anywhere, Action::Bottom, "bottom"),
    bind(
        &[ESC, ch('?'), ch('q'), BACKSPACE],
        Anywhere,
        Action::ToggleHelp,
        "close",
    ),
];

/// Keys and what they do, as the help lists them.
#[derive(Clone, Copy, Debug)]
pub struct Keys {
    pub keys: &'static [Key],
    pub does: &'static str,
}

const fn keys(keys: &'static [Key], does: &'static str) -> Keys {
    Keys { keys, does }
}

/// Keys every mode has, before any other map.
pub const EVERYWHERE: &[Keys] = &[
    keys(
        &[ctrl('x')],
        "leader: the next key is a NAVIGATE key, from any mode",
    ),
    keys(&[ctrl('p')], "command palette"),
    keys(&[ctrl('c')], "interrupt the running turn; twice quits"),
];

/// NAVIGATE keys [`crate::action::for_key`] gives before [`NAVIGATE`].
const TRANSCRIPT_EXTRAS: &[Keys] = &[
    keys(DIGITS, "pick an answer to the pending question"),
    keys(&[ENTER], "on an item: expand it"),
];
const SIDEBAR_EXTRAS: &[Keys] = &[keys(
    &[ESC, BACKSPACE],
    "back to the open session (⌫ on a phone)",
)];

/// The composer's keys; the rest type.
pub const COMPOSER: &[Keys] = &[
    keys(&[ENTER], "send"),
    keys(&[alt(KeyCode::Enter), ctrl('j')], "new line"),
    keys(&[ch('/')], "at the start: commands; // sends a literal /"),
    keys(&[ch('@')], "mention a task child, or a file"),
    keys(
        &[ctrl('v'), cmd('v')],
        "attach the clipboard's image (cmd+v: Ghostty 1.3.1+)",
    ),
    keys(&[UP, DOWN], "earlier prompts; in a popup, move"),
    keys(&[ctrl('p'), ctrl('n')], "in a popup: move"),
    keys(&[TAB], "in a popup: complete; else switch pane"),
    keys(&[BACKTAB], "switch pane"),
    keys(DIGITS, "with nothing typed: pick a pending answer"),
    keys(&[PGUP, PGDN], "scroll the transcript"),
    keys(&[ESC], "close a popup, else leave the composer"),
    keys(
        &[BACKSPACE],
        "with nothing typed: drop the last image, else leave",
    ),
];

/// The keys while an approval takes the composer's place.
pub const APPROVAL: &[Keys] = &[
    keys(&[ch('y')], "allow"),
    keys(&[ch('n')], "deny"),
    keys(&[LEFT, RIGHT, ch('h'), ch('l')], "choose allow or deny"),
    keys(&[ENTER], "give the chosen answer"),
    keys(&[ch('f')], "the request full screen"),
    keys(&[PGUP, PGDN], "scroll the transcript"),
    keys(&[TAB, BACKTAB], "switch pane"),
    keys(&[ESC, BACKSPACE], "leave"),
];

/// The inbox's keys, with NAVIGATE's for the rest.
pub const INBOX: &[Keys] = &[
    keys(&[ch('k'), UP, ch('j'), DOWN], "move"),
    keys(&[PGUP, PGDN], "move a page"),
    keys(&[ch('g'), HOME, ch('G'), END], "first, last"),
    keys(&[ch('y'), ch('n')], "allow, deny"),
    keys(DIGITS, "pick an answer"),
    keys(&[ENTER, ch('a')], "type an answer; enter sends it"),
    keys(&[ch('l'), ch('o'), RIGHT], "open its session"),
    keys(&[ESC, BACKSPACE, ch('h'), LEFT, ch('i'), ch('I')], "close"),
];

/// The board's keys, with NAVIGATE's for the rest.
pub const BOARD: &[Keys] = &[
    keys(&[ch('k'), UP, ch('j'), DOWN], "move"),
    keys(&[PGUP, PGDN], "move a page"),
    keys(&[ch('g'), HOME, ch('G'), END], "first, last"),
    keys(&[ENTER, ch('l'), ch('o'), RIGHT], "open its session"),
    keys(&[ESC, BACKSPACE, ch('h'), LEFT, ch('B')], "close"),
];

/// The pull request lists' keys, with NAVIGATE's for the rest.
pub const PRS: &[Keys] = &[
    keys(&[ch('k'), UP, ch('j'), DOWN], "move"),
    keys(&[PGUP, PGDN], "move a page"),
    keys(&[ch('g'), HOME, ch('G'), END], "first, last"),
    keys(&[ENTER, ch('o')], "open in the browser"),
    keys(&[ch('x')], "unlink"),
    keys(&[ch('l'), RIGHT], "every PR: open its session"),
    keys(&[ESC, BACKSPACE, ch('h'), LEFT], "close"),
];

/// The palette's and the pickers' keys; the rest type a search.
pub const PALETTE: &[Keys] = &[
    keys(&[UP, ctrl('p'), ctrl('k')], "up"),
    keys(&[DOWN, ctrl('n'), ctrl('j')], "down"),
    keys(&[PGUP, PGDN], "a page"),
    keys(&[HOME, END], "first, last"),
    keys(&[ENTER], "run, or pick"),
    keys(&[TAB], "complete"),
    keys(&[ESC], "close"),
    keys(&[BACKSPACE], "with nothing typed: close"),
];

/// The fork dialog's keys.
pub const FORK: &[Keys] = &[
    keys(&[ch('k'), UP, ch('j'), DOWN], "move"),
    keys(&[HOME, PGUP, END, PGDN], "first, last"),
    keys(&[ENTER], "fork onto the host"),
    keys(&[ESC, BACKSPACE, ch('q'), ch('F')], "close"),
];

/// The accounts screen's keys.
pub const ACCOUNTS: &[Keys] = &[
    keys(&[ch('k'), UP, ch('j'), DOWN], "move"),
    keys(&[ch('g'), HOME, PGUP, ch('G'), END, PGDN], "first, last"),
    keys(&[ch('n')], "add an account"),
    keys(&[ch('l')], "log in again"),
    keys(&[ch('r')], "reconnect"),
    keys(&[ch('?')], "this help"),
    keys(&[ESC, BACKSPACE, LEFT, ch('h'), ch('q'), ch('A')], "close"),
];

/// The fleet's keys.
pub const FLEET: &[Keys] = &[
    keys(&[ch('k'), UP, ch('j'), DOWN], "move"),
    keys(&[HOME, PGUP, END, PGDN], "first, last"),
    keys(&[ch('a')], "add a machine"),
    keys(&[ch('n')], "add an account"),
    keys(&[ch('e')], "rename the machine on this device"),
    keys(&[ch('d')], "forget the machine"),
    keys(&[ch('b')], "backups"),
    keys(&[ch('s')], "a vault's secrets"),
    keys(&[ch('r')], "reconnect"),
    keys(&[ch('?')], "this help"),
    keys(&[ESC, BACKSPACE, ch('m'), ch('q')], "close"),
];

/// A row of the help: its keys, or for a gesture or a command its text, and what it does.
pub struct Row {
    pub keys: Vec<Key>,
    pub text: Option<String>,
    pub does: String,
}

impl Row {
    /// The keys as written, or the text.
    pub fn label(&self) -> String {
        self.text.clone().unwrap_or_else(|| names(&self.keys))
    }
}

/// A heading of the help and its rows.
pub struct Section {
    pub title: &'static str,
    pub rows: Vec<Row>,
}

impl Section {
    fn new(title: &'static str) -> Self {
        Self {
            title,
            rows: Vec::new(),
        }
    }

    fn row(mut self, keys: &[Key], does: &str) -> Self {
        self.rows.push(Row {
            keys: keys.to_vec(),
            text: None,
            does: does.to_owned(),
        });
        self
    }

    fn keys(self, list: &[Keys]) -> Self {
        list.iter()
            .fold(self, |section, row| section.row(row.keys, row.does))
    }

    /// The binds of `binds` whose [`On`] `pick` takes.
    fn binds(self, binds: &[Bind], pick: impl Fn(On) -> bool) -> Self {
        binds
            .iter()
            .filter(|bind| pick(bind.on))
            .fold(self, |section, bind| section.row(bind.keys, bind.does))
    }

    /// A row of no key: a gesture, or a command.
    fn text(mut self, label: String, does: String) -> Self {
        self.rows.push(Row {
            keys: Vec::new(),
            text: Some(label),
            does,
        });
        self
    }
}

/// `keys` as the help writes them: `1-9`, or each name, comma-separated.
pub fn names(keys: &[Key]) -> String {
    if keys == DIGITS {
        return "1-9".to_owned();
    }
    keys.iter().map(Key::name).collect::<Vec<_>>().join(", ")
}

/// Every section of the help, in order.
pub fn sections() -> Vec<Section> {
    // What works where `focus` is, but not everywhere: NAVIGATE's section has the rest.
    let only = |focus: Focus| move |on: On| on != Anywhere && on.holds(focus);
    let commands = COMMANDS.iter().fold(
        Section::new("commands: after :, ctrl+p, or / in the composer"),
        |section, command| {
            let label = format!("{} {}", command.name, command.args);
            let key = crate::palette::key_of(command);
            let does = if key.is_empty() {
                command.does.to_owned()
            } else {
                format!("{} ({key})", command.does)
            };
            section.text(label.trim_end().to_owned(), does)
        },
    );
    vec![
        Section::new("everywhere").keys(EVERYWHERE),
        Section::new("navigate").binds(NAVIGATE, |on| on == Anywhere),
        Section::new("sidebar")
            .keys(SIDEBAR_EXTRAS)
            .binds(NAVIGATE, only(Focus::Sessions)),
        Section::new("session")
            .keys(TRANSCRIPT_EXTRAS)
            .binds(NAVIGATE, only(Focus::Transcript)),
        Section::new("composer").keys(COMPOSER),
        Section::new("approval").keys(APPROVAL),
        Section::new("leader: ctrl+x, then")
            .row(DIGITS, "open that row of the attention list")
            .binds(LEADER, |_| true),
        Section::new("inbox").keys(INBOX),
        Section::new("board").keys(BOARD),
        Section::new("pull requests").keys(PRS),
        Section::new("palette and pickers").keys(PALETTE),
        Section::new("terminals")
            .binds(TERMINALS, |_| true)
            .row(&[ctrl(']')], "then d: detach from an attached terminal"),
        Section::new("fork").keys(FORK),
        Section::new("accounts").keys(ACCOUNTS),
        Section::new("fleet").keys(FLEET),
        commands,
        Section::new("mouse")
            .text(
                "tap, swipe".to_owned(),
                "open, press the buttons, scroll".to_owned(),
            )
            .text(
                "shift+drag".to_owned(),
                "select text while herder has the mouse".to_owned(),
            ),
        Section::new("this help").binds(HELP, |_| true),
    ]
}

#[cfg(test)]
mod tests {
    use herder_protocol::ItemBody;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;
    use crate::action::{self, Action};
    use crate::app::{App, Msg};
    use crate::fake::{self, added, update};

    fn press(app: &mut App, code: KeyCode) {
        app.update(Msg::Key(KeyEvent::new(code, KeyModifiers::NONE)));
    }

    /// Every key worth pressing: each printable character, Ctrl and each letter, Alt-Enter,
    /// Cmd-V, and the named keys.
    fn every_key() -> Vec<KeyEvent> {
        let mut keys: Vec<KeyEvent> = (' '..='~')
            .map(|c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))
            .chain(('a'..='z').map(|c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)))
            .collect();
        keys.push(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
        keys.push(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::SUPER));
        keys.extend(
            [
                KeyCode::Enter,
                KeyCode::Esc,
                KeyCode::Tab,
                KeyCode::BackTab,
                KeyCode::Backspace,
                KeyCode::Up,
                KeyCode::Down,
                KeyCode::Left,
                KeyCode::Right,
                KeyCode::PageUp,
                KeyCode::PageDown,
                KeyCode::Home,
                KeyCode::End,
            ]
            .map(|code| KeyEvent::new(code, KeyModifiers::NONE)),
        );
        keys
    }

    /// Whether `action` is a key typed into a field, not a key of the map.
    fn types(action: Action) -> bool {
        matches!(
            action,
            Action::Compose(Act::Key(_))
                | Action::Palette(crate::palette::Input::Key(_))
                | Action::NewSession(crate::new_session::Input::Key(_))
                | Action::Switch(crate::switch::Input::Char(_))
                | Action::Machines(crate::machines::Input::Char(_))
                | Action::Pr(PrAction::Type(_))
                | Action::Inbox(InboxAction::Key(_))
        )
    }

    /// Asserts every key bound in `app`'s state, by `dispatch`, is listed in the help under
    /// one of `titles`.
    fn listed(app: &App, titles: &[&str], dispatch: impl Fn(KeyEvent, &App) -> Option<Action>) {
        let sections = sections();
        let rows: Vec<&Row> = sections
            .iter()
            .filter(|section| titles.contains(&section.title))
            .flat_map(|section| &section.rows)
            .collect();
        assert!(!rows.is_empty(), "no section among {titles:?}");
        for key in every_key() {
            if !dispatch(key, app).is_some_and(|action| !types(action)) {
                continue;
            }
            assert!(
                rows.iter()
                    .any(|row| row.keys.iter().any(|bound| bound.matches(key))),
                "{key:?} is not in the help under {titles:?}",
            );
        }
    }

    fn check(app: &App, titles: &[&str]) {
        listed(app, titles, action::for_key);
    }

    const NAV: [&str; 2] = ["everywhere", "navigate"];

    #[test]
    fn every_navigate_key_is_in_the_help() {
        let mut app = fake::tree();
        check(&app, &[NAV[0], NAV[1], "sidebar"]);
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.focus, Focus::Transcript);
        check(&app, &[NAV[0], NAV[1], "session"]);
    }

    #[test]
    fn every_leader_key_is_in_the_help() {
        let app = fake::tree();
        listed(&app, &["leader: ctrl+x, then"], |key, _| {
            action::for_leader(key)
        });
    }

    #[test]
    fn every_composer_and_approval_key_is_in_the_help() {
        let mut app = fake::tree();
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.focus, Focus::Composer);
        check(&app, &["everywhere", "composer"]);
        let mut app = fake::tree();
        fake::feed(
            &mut app,
            "h1",
            "s2",
            update(
                "s2",
                3,
                vec![
                    fake::started("turn-1"),
                    added(
                        "i1",
                        ItemBody::UserMessage {
                            text: "Clean the build.".into(),
                            attachments: Vec::new(),
                        },
                    ),
                    fake::approval("a1", "Bash: rm -rf target"),
                ],
                Vec::new(),
            ),
        );
        press(&mut app, KeyCode::Enter);
        // An asking session opens in APPROVAL; `i` gives the approval the composer's place.
        assert_eq!(app.focus, Focus::Transcript);
        press(&mut app, KeyCode::Char('i'));
        assert_eq!(app.focus, Focus::Composer);
        check(&app, &["everywhere", "approval"]);
    }

    #[test]
    fn every_key_of_the_inbox_board_and_prs_is_in_the_help() {
        for (key, title) in [('I', "inbox"), ('B', "board")] {
            let mut app = fake::tree();
            press(&mut app, KeyCode::Char(key));
            check(&app, &[NAV[0], NAV[1], title]);
        }
        let mut app = fake::with_prs();
        press(&mut app, KeyCode::Char('P'));
        assert_eq!(app.focus, Focus::AllPrs);
        check(&app, &[NAV[0], NAV[1], "pull requests"]);
    }

    #[test]
    fn every_key_of_the_dialogs_is_in_the_help() {
        for (key, title, open) in [
            (
                ':',
                "palette and pickers",
                (|app| app.compose.palette.is_some()) as fn(&App) -> bool,
            ),
            ('t', "terminals", |app| app.terminals.is_some()),
            ('F', "fork", |app| app.fork.is_some()),
            ('A', "accounts", |app| app.account_screen.is_some()),
            ('m', "fleet", |app| app.machine_panel.is_some()),
        ] {
            let mut app = fake::tree();
            press(&mut app, KeyCode::Char(key));
            assert!(open(&app), "{key} opened no {title}");
            check(&app, &["everywhere", title]);
        }
    }

    #[test]
    fn the_help_closes_only_with_its_keys() {
        let mut app = fake::tree();
        press(&mut app, KeyCode::Char('?'));
        assert!(app.help);
        check(&app, &["everywhere", "this help"]);
        press(&mut app, KeyCode::Char('x'));
        press(&mut app, KeyCode::Enter);
        assert!(app.help, "only esc, ?, q and ⌫ close the help");
        for close in [
            KeyCode::Esc,
            KeyCode::Char('?'),
            KeyCode::Char('q'),
            KeyCode::Backspace,
        ] {
            if !app.help {
                press(&mut app, KeyCode::Char('?'));
            }
            press(&mut app, close);
            assert!(!app.help, "{close:?} leaves the help open");
        }
    }

    #[test]
    fn every_command_is_in_the_help() {
        let sections = sections();
        let commands = sections
            .iter()
            .find(|section| section.title.starts_with("commands"))
            .map(|section| &section.rows)
            .unwrap();
        for command in COMMANDS {
            assert!(
                commands
                    .iter()
                    .any(|row| row.label().split(' ').next() == Some(command.name)),
                "/{} is not in the help",
                command.name
            );
        }
    }

    #[test]
    fn keys_are_named_as_the_bar_names_them() {
        assert_eq!(names(&[ctrl('u'), PGUP, ch(' ')]), "ctrl+u, pgup, space");
        assert_eq!(
            names(&[alt(KeyCode::Enter), BACKTAB]),
            "alt+enter, shift+tab"
        );
        assert_eq!(names(DIGITS), "1-9");
    }
}
