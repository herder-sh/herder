//! The prompt editor's own behaviour, as OpenCode's (docs/tui-design.md §4.4, §5.2):
//!
//! - `/` at the start opens command completion; a `/command` line runs instead of being
//!   sent, and `//` sends a literal `/`.
//! - `@` opens mention completion: the session's task children, by branch, then the folders
//!   and images of this machine, from the working directory. Picking an image attaches it;
//!   so does sending an `@path` to one.
//! - `↑` on the first line and `↓` on the last walk the prompt history: the session's own
//!   prompts first, then those sent to other sessions.
//! - A long paste collapses to `[pasted ~N lines]`, and is sent in full.

use herder_protocol::CommandBody;

use crate::action::Action;
use crate::app::{App, Effect};
use crate::compose::Origin;
use crate::inbox::InboxAction;
use crate::prs::{PrAction, parse_number};
use crate::session::{Entry, SessionKey};
use crate::ui::glyphs::Glyphs;

/// A `/` command.
#[derive(Debug, PartialEq, Eq)]
pub struct Command {
    pub name: &'static str,
    /// Its arguments, as the popup shows them; empty when it takes none.
    pub args: &'static str,
    pub does: &'static str,
}

const fn command(name: &'static str, args: &'static str, does: &'static str) -> Command {
    Command { name, args, does }
}

/// Every `/` command, in the order the popup lists them.
pub const COMMANDS: &[Command] = &[
    command("new", "", "new session"),
    command("model", "<name>", "switch the model"),
    command("mode", "<mode>", "read_only, ask, auto_edit or full_access"),
    command("switch", "", "switch account, provider or model"),
    command("stop", "", "interrupt the running turn"),
    command("rename", "<title>", "rename the session"),
    command(
        "retitle",
        "",
        "ask AI to title the session from its conversation",
    ),
    command("archive", "", "archive the session"),
    command("unarchive", "", "bring an archived session back"),
    command("pr", "<n|url>", "link a pull request"),
    command("unpr", "[n]", "unlink a pull request"),
    command("term", "", "terminals (owners)"),
    command("down", "[project]", "stop the session's compose project"),
    command("fork", "", "fork a session onto a host"),
    command("thinking", "", "show or hide reasoning"),
    command("details", "", "show or hide tool output"),
    command("glyphs", "ascii|unicode", "plain marks, or symbols"),
    command(
        "mouse",
        "on|off",
        "take the mouse, or give it to the terminal",
    ),
    command("inbox", "", "everything waiting on you"),
    command("prs", "", "every session's pull requests"),
    command("accounts", "", "accounts and their usage"),
    command("fleet", "", "machines"),
    command("add", "", "add a machine"),
    command("reconnect", "", "reconnect every machine now"),
    command("group", "", "group sessions by project or by machine"),
    command("help", "", "keys"),
    command("quit", "", "leave herder"),
];

/// Lines a paste must reach, or characters exceed, to collapse.
const PASTE_LINES: usize = 3;
const PASTE_CHARS: usize = 150;

/// A completion the popup offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Completion {
    /// What replaces the word being typed.
    pub insert: String,
    /// What the popup shows on the left.
    pub label: String,
    /// What it does, or what it names.
    pub does: String,
    /// A command that takes no arguments: accepting it runs it.
    pub runs: bool,
    /// An image file, by its path: accepting it attaches it.
    pub attach: Option<String>,
}

/// Where the history walk is: the entry shown, and the text written before it started.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recall {
    pub at: usize,
    pub draft: String,
}

/// A path as typed split after its last `/`: the folder, and the start of a name in it.
fn split_path(path: &str) -> (&str, &str) {
    match path.rfind('/') {
        Some(at) => path.split_at(at + 1),
        None => ("", path),
    }
}

/// Whether `query`'s characters appear in `name` in order.
fn fuzzy(name: &str, query: &str) -> bool {
    let mut name = name.chars();
    query
        .chars()
        .all(|q| name.any(|c| c.eq_ignore_ascii_case(&q)))
}

impl App {
    /// The editor's text, lines joined.
    fn prompt_text(&self) -> String {
        self.compose.editor.lines().join("\n")
    }

    /// The word the cursor ends, with where it starts on its line: what a completion
    /// replaces.
    fn word_at_cursor(&self) -> Option<(usize, usize, String)> {
        let ratatui_textarea::DataCursor(row, col) = self.compose.editor.cursor();
        let line = self.compose.editor.lines().get(row)?;
        let before: String = line.chars().take(col).collect();
        let start = before
            .char_indices()
            .rev()
            .find(|(_, c)| c.is_whitespace())
            .map_or(0, |(at, c)| at + c.len_utf8());
        Some((
            row,
            before[..start].chars().count(),
            before[start..].to_owned(),
        ))
    }

    /// What the popup offers for the word at the cursor: commands after a `/` that starts
    /// the prompt, the session's children after an `@`.
    pub fn completions(&self) -> Vec<Completion> {
        if self.compose.popup_hidden.as_deref() == Some(self.prompt_text().as_str()) {
            return Vec::new();
        }
        let Some((row, start, word)) = self.word_at_cursor() else {
            return Vec::new();
        };
        if let Some(query) = word.strip_prefix('/')
            && row == 0
            && start == 0
            && !query.starts_with('/')
        {
            let mut found: Vec<&Command> = COMMANDS
                .iter()
                .filter(|command| fuzzy(command.name, query))
                .collect();
            // Prefix matches first.
            found.sort_by_key(|command| !command.name.starts_with(query));
            return found
                .into_iter()
                .map(|command| Completion {
                    insert: format!("/{} ", command.name),
                    label: if command.args.is_empty() {
                        format!("/{}", command.name)
                    } else {
                        format!("/{} {}", command.name, command.args)
                    },
                    does: command.does.to_owned(),
                    runs: command.args.is_empty(),
                    attach: None,
                })
                .collect();
        }
        if let Some(query) = word.strip_prefix('@')
            && let Some(key) = &self.open
        {
            let mut children: Vec<Completion> = self
                .children(key)
                .into_iter()
                .filter_map(|child| self.sessions.get(child))
                .filter(|child| !child.branch.is_empty())
                .filter(|child| fuzzy(&child.branch, query) || fuzzy(&child.title(), query))
                .map(|child| Completion {
                    insert: format!("@{} ", child.branch),
                    label: format!("@{}", child.branch),
                    does: child.title(),
                    runs: false,
                    attach: None,
                })
                .collect();
            children.sort_by(|a, b| a.label.cmp(&b.label));
            children.extend(self.file_completions(query));
            return children;
        }
        Vec::new()
    }

    /// The folders and images of the listed folder that `query`, a path after an `@`, may
    /// go on to name: those it starts first; hidden ones only for a query starting with `.`.
    fn file_completions(&self, query: &str) -> Vec<Completion> {
        let (dir, stem) = split_path(query);
        let Some((_, entries)) = self
            .compose
            .listing
            .as_ref()
            .filter(|(listed, _)| listed == dir)
        else {
            return Vec::new();
        };
        let mut found: Vec<Completion> = entries
            .iter()
            .filter(|entry| !entry.name.starts_with('.') || stem.starts_with('.'))
            .filter(|entry| fuzzy(&entry.name, stem))
            .map(|entry| {
                let path = format!("{dir}{}", entry.name);
                if entry.is_dir {
                    Completion {
                        insert: format!("@{path}/"),
                        label: format!("@{path}/"),
                        does: "folder".to_owned(),
                        runs: false,
                        attach: None,
                    }
                } else {
                    Completion {
                        insert: String::new(),
                        label: format!("@{path}"),
                        does: "attach image".to_owned(),
                        runs: false,
                        attach: Some(path),
                    }
                }
            })
            .collect();
        let starts = |c: &Completion| {
            let name = c.label.trim_start_matches('@')[dir.len()..].to_lowercase();
            name.starts_with(&stem.to_lowercase())
        };
        found.sort_by_key(|c| !starts(c));
        found
    }

    /// The folder to list for the `@` path at the cursor, when it is not listed yet.
    pub(crate) fn list_wanted(&mut self) -> Vec<Effect> {
        let Some((_, _, word)) = self.word_at_cursor() else {
            return Vec::new();
        };
        let Some(query) = word.strip_prefix('@') else {
            return Vec::new();
        };
        let (dir, _) = split_path(query);
        if self
            .compose
            .listing
            .as_ref()
            .is_some_and(|(listed, _)| listed == dir)
        {
            return Vec::new();
        }
        // Listed as asked: a second ask before the answer finds it there.
        self.compose.listing = Some((dir.to_owned(), Vec::new()));
        vec![Effect::List(dir.to_owned())]
    }

    /// Puts the popup's selected completion in place of the word at the cursor: a command
    /// without arguments runs, an image attaches, a folder lists.
    pub(crate) fn accept_completion(&mut self) -> Vec<Effect> {
        let completions = self.completions();
        let Some(completion) = completions
            .get(self.compose.popup.min(completions.len().saturating_sub(1)))
            .cloned()
        else {
            return Vec::new();
        };
        let Some((_, _, word)) = self.word_at_cursor() else {
            return Vec::new();
        };
        let editor = &mut self.compose.editor;
        for _ in word.chars() {
            editor.delete_char();
        }
        editor.insert_str(&completion.insert);
        self.compose.popup = 0;
        if completion.runs {
            return self.submit();
        }
        if let Some(path) = completion.attach {
            return self.attach(crate::attach::Source::File(path), None);
        }
        self.list_wanted()
    }

    /// Moves the popup's selection by `step`, wrapping.
    pub(crate) fn move_completion(&mut self, step: isize) {
        let len = self.completions().len();
        if len > 0 {
            let at = self.compose.popup.min(len - 1);
            self.compose.popup = at.saturating_add_signed(step + len as isize) % len;
        }
    }

    /// The prompts `↑` walks back through, newest first: the open session's, then the rest
    /// sent from here.
    fn history(&self) -> Vec<String> {
        let mut history: Vec<String> = Vec::new();
        let mut add = |text: &str| {
            if !text.trim().is_empty() && !history.iter().any(|known| known == text) {
                history.push(text.to_owned());
            }
        };
        let open = self.open.as_ref();
        let mine = |key: &SessionKey| Some(key) == open;
        for (_, text) in self
            .compose
            .history
            .iter()
            .rev()
            .filter(|(key, _)| mine(key))
        {
            add(text);
        }
        if let Some(session) = self.open_session() {
            for entry in session.entries.iter().rev() {
                if let Entry::Item(item) = entry
                    && let herder_protocol::ItemBody::UserMessage { text, .. } = &item.body
                {
                    add(text);
                }
            }
        }
        for (_, text) in self
            .compose
            .history
            .iter()
            .rev()
            .filter(|(key, _)| !mine(key))
        {
            add(text);
        }
        history
    }

    /// Walks the history back (`step` 1) or forward; returns whether it moved.
    pub(crate) fn recall(&mut self, step: isize) -> bool {
        let history = self.history();
        let next = match (&self.compose.recall, step) {
            (None, 1) => Some(0),
            (None, _) => return false,
            (Some(recall), 1) => Some((recall.at + 1).min(history.len().saturating_sub(1))),
            (Some(recall), _) => recall.at.checked_sub(1),
        };
        let text = match next {
            Some(at) => {
                let Some(text) = history.get(at).cloned() else {
                    return false;
                };
                let draft = match self.compose.recall.take() {
                    Some(recall) => recall.draft,
                    None => self.prompt_text(),
                };
                self.compose.recall = Some(Recall { at, draft });
                text
            }
            // Past the newest: back to what was being written.
            None => match self.compose.recall.take() {
                Some(recall) => recall.draft,
                None => return false,
            },
        };
        self.compose.set_text(&text);
        true
    }

    /// Inserts a paste into the prompt; a long one as `[pasted ~N lines]`.
    pub(crate) fn paste_prompt(&mut self, text: &str) {
        let text = text.replace("\r\n", "\n");
        let lines = text.lines().count();
        if lines < PASTE_LINES && text.chars().count() <= PASTE_CHARS {
            self.compose.editor.insert_str(text);
            return;
        }
        let mut token = format!("[pasted ~{lines} lines]");
        let same = self
            .compose
            .pastes
            .iter()
            .filter(|(known, _)| known.starts_with(token.trim_end_matches(']')))
            .count();
        if same > 0 {
            token = format!("[pasted ~{lines} lines #{}]", same + 1);
        }
        self.compose.editor.insert_str(&token);
        self.compose.pastes.push((token, text));
    }

    /// `text` with each collapsed paste in full.
    pub(crate) fn expand_pastes(&self, text: &str) -> String {
        let mut text = text.to_owned();
        for (token, paste) in &self.compose.pastes {
            text = text.replace(token, paste);
        }
        text
    }

    /// Runs the `/command` line `line` on the open session `key`: the effects, or why it
    /// cannot run.
    pub(crate) fn slash(
        &mut self,
        target: Option<&SessionKey>,
        line: &str,
    ) -> Result<Vec<Effect>, String> {
        // The commands on a session name it; the rest run without one, as from the palette.
        let session = || target.ok_or_else(|| "no session selected".to_owned());
        let mut words = line.trim_start_matches('/').split_whitespace();
        let name = words.next().unwrap_or("");
        let args: Vec<&str> = words.collect();
        let open = |app: &mut App, action| Ok(app.act(action));
        match (name, args.as_slice()) {
            ("new", []) => open(self, Action::Compose(crate::compose::Act::NewSession)),
            ("switch", []) => open(self, Action::OpenSwitch),
            ("term", []) => open(self, Action::Terminals),
            ("fork", []) => open(self, Action::OpenFork),
            ("inbox", []) => open(self, Action::Inbox(InboxAction::Toggle)),
            ("prs", []) => open(self, Action::Pr(PrAction::ToggleAll)),
            ("accounts", []) => open(self, Action::OpenAccounts),
            ("fleet", []) => open(self, Action::OpenMachines),
            ("help", []) => open(self, Action::ToggleHelp),
            ("add", []) => open(self, Action::AddMachine),
            ("reconnect", []) => open(self, Action::Reconnect),
            ("group", []) => open(self, Action::Group),
            ("quit", []) => Ok(vec![Effect::Quit]),
            ("thinking", []) => {
                self.chat.thinking = !self.chat.thinking;
                Ok(Vec::new())
            }
            ("details", []) => {
                self.chat.details = !self.chat.details;
                Ok(Vec::new())
            }
            ("glyphs", [glyphs]) => match Glyphs::parse(glyphs) {
                Some(glyphs) => {
                    self.glyphs = Some(glyphs);
                    Ok(vec![Effect::Save])
                }
                None => Err("usage: /glyphs ascii|unicode".to_owned()),
            },
            ("glyphs", _) => Err("usage: /glyphs ascii|unicode".to_owned()),
            ("mouse", [on @ ("on" | "off")]) => {
                let on = *on == "on";
                self.mouse = on;
                Ok(vec![Effect::Mouse(on), Effect::Save])
            }
            ("mouse", _) => Err("usage: /mouse on|off".to_owned()),
            ("pr", [pr]) => {
                let key = session()?;
                let number = parse_number(pr).ok_or_else(|| format!("not a pull request: {pr}"))?;
                let command = CommandBody::LinkPr {
                    session_id: key.session_id.clone(),
                    number,
                };
                Ok(vec![send(key, command)])
            }
            ("pr", _) => Err("usage: /pr <number or URL>".to_owned()),
            ("unpr", args) => {
                let key = session()?;
                let prs = self
                    .sessions
                    .get(key)
                    .map(|session| session.prs.iter().map(|pr| pr.number).collect::<Vec<_>>())
                    .unwrap_or_default();
                let number = match (args, prs.as_slice()) {
                    ([pr], _) => {
                        parse_number(pr).ok_or_else(|| format!("not a pull request: {pr}"))?
                    }
                    ([], [only]) => *only,
                    ([], []) => return Err("no pull request is linked".to_owned()),
                    _ => return Err("usage: /unpr <number>".to_owned()),
                };
                let command = CommandBody::UnlinkPr {
                    session_id: key.session_id.clone(),
                    number,
                };
                Ok(vec![send(key, command)])
            }
            ("rename", [_, ..]) => {
                let key = session()?;
                // The title as typed, its inner spaces kept.
                let typed = line.trim_start_matches('/').trim_start()["rename".len()..].trim();
                let title = herder_protocol::clean_title(typed).ok_or_else(|| {
                    format!(
                        "a title is one line of at most {} characters",
                        herder_protocol::MAX_TITLE_CHARS
                    )
                })?;
                let command = CommandBody::RenameSession {
                    session_id: key.session_id.clone(),
                    title: title.to_owned(),
                };
                Ok(vec![send(key, command)])
            }
            ("retitle", []) => {
                let key = session()?;
                let command = CommandBody::RetitleSession {
                    session_id: key.session_id.clone(),
                };
                Ok(vec![send(key, command)])
            }
            ("stop", []) => self.session_command(session()?, "interrupt", &[]),
            ("model" | "mode" | "archive" | "unarchive" | "down", args) => {
                self.session_command(session()?, name, args)
            }
            _ => match COMMANDS.iter().find(|command| command.name == name) {
                Some(command) => Err(format!("usage: /{} {}", command.name, command.args)),
                None => Err(format!("unknown command: /{name}")),
            },
        }
    }

    fn session_command(
        &mut self,
        key: &SessionKey,
        name: &str,
        args: &[&str],
    ) -> Result<Vec<Effect>, String> {
        let (key, body) = self
            .command(Some(key), name, args)
            .map_err(|error| error.replacen("usage: ", "usage: /", 1))?;
        Ok(vec![send(&key, body)])
    }
}

fn send(key: &SessionKey, command: CommandBody) -> Effect {
    Effect::Send {
        host_id: key.host_id.clone(),
        command,
        origin: Origin::Session(key.clone()),
    }
}

#[cfg(test)]
mod tests {
    use herder_protocol::{ApprovalDecision, CommandBody, PermissionMode, SessionId};
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use crate::action::Action;
    use crate::app::{App, Effect, Focus, Msg, Row};
    use crate::compose::{Act, Origin};
    use crate::fake::{self, approval, key, question, started, type_text, update};

    fn press(app: &mut App, code: KeyCode) -> Vec<Effect> {
        app.update(Msg::Key(KeyEvent::new(code, KeyModifiers::NONE)))
    }

    /// [`fake::tree`] with `s2` open, `bodies` fed to it, writing in its prompt.
    fn writing(bodies: Vec<herder_protocol::EventBody>) -> App {
        let mut app = fake::tree();
        fake::feed(&mut app, "h1", "s2", update("s2", 3, bodies, Vec::new()));
        app.choose_row(Row::Session {
            key: key("h1", "s2"),
            depth: 0,
        });
        app.act(Action::Open);
        app.act(Action::Compose(Act::Write));
        assert_eq!(app.focus, Focus::Composer);
        app
    }

    fn on_s2(command: CommandBody) -> Effect {
        Effect::Send {
            host_id: key("h1", "s2").host_id,
            command,
            origin: Origin::Session(key("h1", "s2")),
        }
    }

    fn text(app: &App) -> String {
        app.compose.editor.lines().join("\n")
    }

    fn labels(app: &App) -> Vec<String> {
        app.completions().into_iter().map(|c| c.label).collect()
    }

    #[test]
    fn slash_completes_commands_and_runs_them() {
        let mut app = writing(vec![]);
        type_text(&mut app, "/mo");
        assert_eq!(
            labels(&app)[..3],
            ["/model <name>", "/mode <mode>", "/mouse on|off"]
        );
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Tab);
        assert_eq!(text(&app), "/mode ");
        assert!(labels(&app).is_empty());
        type_text(&mut app, "auto_edit");
        let effects = press(&mut app, KeyCode::Enter);
        assert_eq!(
            effects,
            [on_s2(CommandBody::SetPermissionMode {
                session_id: SessionId::new("s2"),
                mode: PermissionMode::AutoEdit,
            })]
        );
        assert_eq!(text(&app), "");

        // A command without arguments runs as it is picked.
        type_text(&mut app, "/think");
        assert!(app.chat.thinking);
        assert_eq!(press(&mut app, KeyCode::Enter), []);
        assert!(!app.chat.thinking);
        assert_eq!(text(&app), "");

        // Esc hides the popup until the text changes.
        type_text(&mut app, "/st");
        press(&mut app, KeyCode::Esc);
        assert!(labels(&app).is_empty());
        assert_eq!(app.focus, Focus::Composer);
        type_text(&mut app, "o");
        assert_eq!(labels(&app), ["/stop"]);
    }

    #[test]
    fn a_bad_command_keeps_its_text_and_says_why() {
        let mut app = writing(vec![]);
        type_text(&mut app, "/bogus");
        assert!(labels(&app).is_empty());
        assert_eq!(press(&mut app, KeyCode::Enter), []);
        assert_eq!(text(&app), "/bogus");
        assert_eq!(
            app.compose.errors.get(&key("h1", "s2")).map(String::as_str),
            Some("unknown command: /bogus")
        );
        app.compose.clear();
        type_text(&mut app, "/model");
        // Enter takes the completion, then runs it.
        press(&mut app, KeyCode::Enter);
        assert_eq!(text(&app), "/model ");
        press(&mut app, KeyCode::Enter);
        assert_eq!(
            app.compose.errors.get(&key("h1", "s2")).map(String::as_str),
            Some("usage: /model <name>")
        );
    }

    #[test]
    fn a_double_slash_sends_a_literal_slash() {
        let mut app = writing(vec![]);
        type_text(&mut app, "//etc is fine");
        let effects = press(&mut app, KeyCode::Enter);
        assert_eq!(
            effects,
            [Effect::Send {
                host_id: key("h1", "s2").host_id,
                command: CommandBody::SendPrompt {
                    session_id: SessionId::new("s2"),
                    text: "/etc is fine".into(),
                    images: Vec::new(),
                },
                origin: Origin::Prompt(key("h1", "s2"), "/etc is fine".into()),
            }]
        );
    }

    #[test]
    fn at_mentions_the_sessions_children_by_branch() {
        let mut app = writing(vec![]);
        type_text(&mut app, "see @tests");
        assert_eq!(labels(&app), ["@herder/api-tests"]);
        press(&mut app, KeyCode::Tab);
        assert_eq!(text(&app), "see @herder/api-tests ");
        type_text(&mut app, "@");
        assert_eq!(labels(&app), ["@herder/api-docs", "@herder/api-tests"]);
    }

    #[test]
    fn up_and_down_walk_the_prompt_history() {
        let mut app = writing(vec![]);
        for prompt in ["one", "two"] {
            type_text(&mut app, prompt);
            press(&mut app, KeyCode::Enter);
        }
        type_text(&mut app, "draft");
        press(&mut app, KeyCode::Up);
        assert_eq!(text(&app), "two");
        press(&mut app, KeyCode::Up);
        assert_eq!(text(&app), "one");
        press(&mut app, KeyCode::Up);
        assert_eq!(text(&app), "one");
        press(&mut app, KeyCode::Down);
        assert_eq!(text(&app), "two");
        press(&mut app, KeyCode::Down);
        assert_eq!(text(&app), "draft");
    }

    #[test]
    fn a_long_paste_collapses_and_is_sent_in_full() {
        let mut app = writing(vec![]);
        type_text(&mut app, "look: ");
        let pasted = "a\nb\nc\nd";
        app.update(Msg::Paste(pasted.into()));
        assert_eq!(text(&app), "look: [pasted ~4 lines]");
        let effects = press(&mut app, KeyCode::Enter);
        assert!(
            matches!(&effects[..], [Effect::Send { command: CommandBody::SendPrompt { text, .. }, .. }] if text == "look: a\nb\nc\nd"),
            "{effects:?}"
        );
        // A short one goes in as it is.
        app.update(Msg::Paste("short".into()));
        assert_eq!(text(&app), "short");
    }

    #[test]
    fn the_approval_panel_takes_arrows_enter_and_f() {
        let mut app = writing(vec![started("turn-1"), approval("a1", "$ rm -rf target/")]);
        // The prompt is gone: letters answer, not type.
        press(&mut app, KeyCode::Char('f'));
        assert!(app.compose.full);
        press(&mut app, KeyCode::Right);
        let effects = press(&mut app, KeyCode::Enter);
        assert_eq!(
            effects,
            [on_s2(CommandBody::AnswerApproval {
                session_id: SessionId::new("s2"),
                approval_id: herder_protocol::ApprovalId::new("a1"),
                decision: ApprovalDecision::Deny,
            })]
        );
        assert_eq!(text(&app), "");
        assert!(!app.compose.full);
        assert_eq!(app.compose.button, 0);
    }

    #[test]
    fn a_digit_in_an_empty_prompt_picks_a_choice() {
        let mut app = writing(vec![
            started("turn-1"),
            question("q1", "Port?", &["80", "443"]),
        ]);
        let effects = press(&mut app, KeyCode::Char('2'));
        assert_eq!(
            effects,
            [on_s2(CommandBody::AnswerQuestion {
                session_id: SessionId::new("s2"),
                question_id: herder_protocol::QuestionId::new("q1"),
                answer: herder_protocol::Answer::Choice { index: 1 },
            })]
        );
        // Once something is typed, digits are text.
        type_text(&mut app, "x2");
        assert_eq!(text(&app), "x2");
    }

    #[test]
    fn e_renames_the_selected_session_and_r_asks_ai_to_title_it() {
        let mut app = fake::tree();
        app.choose_row(Row::Session {
            key: key("h1", "s2"),
            depth: 0,
        });
        // The palette opens on its name, to edit.
        press(&mut app, KeyCode::Char('E'));
        let search = |app: &App| {
            app.compose
                .palette
                .as_ref()
                .map(|palette| palette.search.lines().join(" "))
        };
        assert_eq!(search(&app).as_deref(), Some("rename api"));
        for _ in 0.."api".len() {
            press(&mut app, KeyCode::Backspace);
        }
        type_text(&mut app, "Health  endpoint ");
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            [on_s2(CommandBody::RenameSession {
                session_id: SessionId::new("s2"),
                title: "Health  endpoint".into(),
            })]
        );
        assert!(app.compose.palette.is_none());

        // A title that is too long stays in the palette, saying why.
        press(&mut app, KeyCode::Char('E'));
        press(&mut app, KeyCode::Char('x'));
        type_text(&mut app, &"x".repeat(herder_protocol::MAX_TITLE_CHARS));
        assert_eq!(press(&mut app, KeyCode::Enter), []);
        let error = app.compose.palette.as_ref().and_then(|p| p.error.clone());
        assert_eq!(
            error.as_deref(),
            Some("a title is one line of at most 80 characters")
        );
        press(&mut app, KeyCode::Esc);

        assert_eq!(
            press(&mut app, KeyCode::Char('R')),
            [on_s2(CommandBody::RetitleSession {
                session_id: SessionId::new("s2"),
            })]
        );
    }

    #[test]
    fn a_title_changed_renames_the_session_everywhere() {
        let mut app = fake::tree();
        let title = |app: &App| app.sessions.get(&key("h1", "s2")).map(|s| s.title());
        assert_eq!(title(&app).as_deref(), Some("app · api"));
        let renamed = herder_protocol::EventBody::TitleChanged {
            title: "Health endpoint".into(),
            source: herder_protocol::TitleSource::User,
        };
        fake::feed(
            &mut app,
            "h1",
            "s2",
            update("s2", 3, vec![renamed], Vec::new()),
        );
        assert_eq!(title(&app).as_deref(), Some("app · Health endpoint"));
    }
}
