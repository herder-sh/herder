//! Terminal attach: the picker of a session's terminals, and the loop that hands the local
//! terminal to a daemon PTY until the user detaches or the shell exits.
//!
//! Attaching suspends the TUI: it leaves the alternate screen, keeps raw mode, and pipes bytes
//! both ways, stdin to the terminal's input and its output to stdout, like ssh. Resizes follow
//! SIGWINCH. Ctrl-] then `d` detaches, Ctrl-] twice sends one Ctrl-]. The daemon replays its
//! scrollback on attach, so the screen comes back as it was.

use std::io::Write;
use std::os::fd::AsFd;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use herder_client_core::{Client, Error, Machine, NewAccount, TerminalEvent, TerminalStream};
use herder_protocol::{AccountId, ErrorCode, HostId, Role, SessionId, TerminalId, TerminalPurpose};
use ratatui::crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::crossterm::{cursor, execute};
use tokio::sync::mpsc;

use crate::session::SessionKey;

/// The byte of Ctrl-], which starts a detach.
pub const PREFIX: u8 = 0x1d;

/// Ctrl-] as terminals send it once a program asked for extended keys: the kitty keyboard
/// protocol, and xterm's modifyOtherKeys.
const ENCODED_PREFIX: [&[u8]; 2] = [b"\x1b[93;5u", b"\x1b[27;5;93~"];

/// How long opening or attaching may wait for the machine before giving up.
const ATTACH_TIMEOUT: Duration = Duration::from_secs(10);

/// Undoes what a full-screen program may have left set: the alternate screen, mouse
/// reporting, bracketed paste, extended keys (modifyOtherKeys and kitty flags), a hidden
/// cursor and other modes (soft reset).
const RESET: &[u8] = b"\x1b[?1049l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?2004l\
    \x1b[>4;0m\x1b[=0;1u\x1b[!p\x1b[?25h";

/// What to attach to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// A new shell in the session's worktree.
    New(SessionId),
    /// An open terminal.
    Existing(TerminalId),
    /// A new account's login.
    Login(NewAccount),
    /// The login of an existing account, again.
    LogInAgain(AccountId),
}

/// How an attach ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ended {
    /// The user detached; the shell keeps running.
    Detached,
    /// The shell exited.
    Exited(Option<i32>),
    /// An account's login ended; what herder made of it.
    Login(String),
    /// The attach did not happen or broke off.
    Failed(String),
}

impl Ended {
    /// The line the status bar shows afterwards.
    pub fn notice(&self) -> String {
        match self {
            Ended::Detached => "detached; the terminal keeps running".to_owned(),
            Ended::Exited(Some(code)) => format!("terminal exited with {code}"),
            Ended::Exited(None) => "terminal exited".to_owned(),
            Ended::Login(text) | Ended::Failed(text) => text.clone(),
        }
    }
}

/// The message a member gets for any terminal action.
pub const OWNER_ONLY: &str = "terminals are owner-only";

/// The open picker: which session's terminals, and the selected row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Picker {
    /// The session.
    pub session: SessionKey,
    /// Index into [`rows`].
    pub selected: usize,
}

/// Why a session's terminals cannot be offered, if they cannot.
pub fn refusal(machines: &[Machine], host_id: &HostId) -> Option<&'static str> {
    let machine = machines.iter().find(|m| m.host_id == *host_id)?;
    match machine.role {
        Some(Role::Owner) => None,
        Some(Role::Member) => Some(OWNER_ONLY),
        None => Some("the machine has not connected yet"),
    }
}

/// The picker's rows: a new terminal first, then the session's open terminals, oldest first.
pub fn rows(machines: &[Machine], session: &SessionKey) -> Vec<Target> {
    let open = machines
        .iter()
        .filter(|machine| machine.host_id == session.host_id)
        .flat_map(|machine| &machine.terminals)
        .filter(|terminal| {
            matches!(&terminal.purpose, TerminalPurpose::Shell { session_id }
                if *session_id == session.session_id)
        })
        .map(|terminal| Target::Existing(terminal.terminal_id.clone()));
    std::iter::once(Target::New(session.session_id.clone()))
        .chain(open)
        .collect()
}

/// Splits typed bytes into what goes to the terminal and whether the user detached.
#[derive(Debug, Default)]
pub struct Prefix {
    armed: bool,
}

impl Prefix {
    /// The bytes to send for `typed`, and `true` once Ctrl-] `d` was typed; bytes after it are
    /// dropped.
    pub fn feed(&mut self, typed: &[u8]) -> (Vec<u8>, bool) {
        let mut send = Vec::with_capacity(typed.len());
        let mut rest = typed;
        while let Some((&byte, after)) = rest.split_first() {
            // A program that turned on extended keys makes the terminal send Ctrl-] as an
            // escape sequence; it still starts a detach.
            let encoded = ENCODED_PREFIX.iter().find(|seq| rest.starts_with(seq));
            let byte = match encoded {
                Some(seq) => {
                    rest = &rest[seq.len()..];
                    PREFIX
                }
                None => {
                    rest = after;
                    byte
                }
            };
            if self.armed {
                self.armed = false;
                match byte {
                    b'd' | b'D' => return (send, true),
                    PREFIX => send.push(PREFIX),
                    other => send.extend([PREFIX, other]),
                }
            } else if byte == PREFIX {
                self.armed = true;
            } else {
                send.push(byte);
            }
        }
        (send, false)
    }
}

/// Where stdin goes: key events for the TUI, or, while attached, raw bytes to a terminal.
/// Shared between the input thread and the attach loop.
#[derive(Clone, Default)]
pub struct RawInput(Arc<Mutex<Option<mpsc::UnboundedSender<Vec<u8>>>>>);

impl RawInput {
    /// In raw mode, waits up to `wait` for stdin and forwards what it read; `false` when not
    /// in raw mode, so the caller reads key events instead.
    pub fn forward(&self, wait: Duration) -> bool {
        if self.lock().is_none() {
            return false;
        }
        let stdin = std::io::stdin();
        let mut fds = [nix::poll::PollFd::new(
            stdin.as_fd(),
            nix::poll::PollFlags::POLLIN,
        )];
        let timeout = nix::poll::PollTimeout::try_from(wait).unwrap_or(nix::poll::PollTimeout::MAX);
        if !matches!(nix::poll::poll(&mut fds, timeout), Ok(ready) if ready > 0) {
            return true;
        }
        // Read under the lock, and only if still in raw mode: once `set(None)` returns, no raw
        // read takes bytes meant for the TUI, such as the answer to a cursor position query.
        let raw = self.lock();
        let Some(tx) = raw.as_ref() else {
            return false;
        };
        // Read the descriptor itself: std's buffered stdin would keep bytes poll cannot see.
        let mut buf = [0u8; 16 * 1024];
        if let Ok(read @ 1..) = nix::unistd::read(stdin.as_fd(), &mut buf) {
            let _ = tx.send(buf[..read].to_vec());
        }
        true
    }

    fn lock(&self) -> MutexGuard<'_, Option<mpsc::UnboundedSender<Vec<u8>>>> {
        // Every update is one assignment, so a poisoned value is consistent.
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Switches stdin to raw bytes for `tx`, or back to key events; waits out a raw read in
    /// flight.
    fn set(&self, tx: Option<mpsc::UnboundedSender<Vec<u8>>>) {
        *self.lock() = tx;
    }
}

/// Attaches the local terminal to `target` on `host_id` until the user detaches or the shell
/// exits, then gives the screen back to the TUI.
pub async fn attach(
    client: &Client,
    host_id: &HostId,
    target: Target,
    input: &RawInput,
    terminal: &mut crate::backend::Tui,
) -> Ended {
    let (cols, rows) = ratatui::crossterm::terminal::size().unwrap_or((80, 24));
    let login = matches!(target, Target::Login(_) | Target::LogInAgain(_));
    let stream = match target {
        Target::New(session_id) => {
            tokio::time::timeout(
                ATTACH_TIMEOUT,
                client.open_terminal(host_id.clone(), session_id, cols, rows),
            )
            .await
        }
        Target::Existing(terminal_id) => {
            tokio::time::timeout(
                ATTACH_TIMEOUT,
                client.attach_terminal(host_id.clone(), terminal_id),
            )
            .await
        }
        Target::Login(account) => {
            tokio::time::timeout(
                ATTACH_TIMEOUT,
                client.add_account(host_id.clone(), account, cols, rows),
            )
            .await
        }
        Target::LogInAgain(account_id) => {
            tokio::time::timeout(
                ATTACH_TIMEOUT,
                client.log_in_account(host_id.clone(), account_id, cols, rows),
            )
            .await
        }
    };
    let stream = match stream {
        Ok(Ok(stream)) => stream,
        Ok(Err(err)) => return Ended::Failed(failure(&err)),
        Err(_) => return Ended::Failed("the machine did not answer in time".to_owned()),
    };
    stream.resize(cols, rows);

    let mut stdout = std::io::stdout();
    let banner = format!(
        "\r\nherder: attached to terminal {}; Ctrl-] d detaches\r\n",
        stream.terminal_id()
    );
    if let Err(err) = execute!(stdout, LeaveAlternateScreen, cursor::Show)
        .and_then(|()| write_all(&mut stdout, banner.as_bytes()))
    {
        return Ended::Failed(format!("cannot hand over the screen: {err}"));
    }
    let (tx, typed) = mpsc::unbounded_channel();
    input.set(Some(tx));
    let mut tail = Tail::default();
    let mut ended = pipe(&stream, typed, &mut stdout, &mut tail).await;
    input.set(None);
    if let (true, Ended::Exited(code)) = (login, &ended) {
        ended = Ended::Login(tail.outcome().unwrap_or_else(|| match code {
            Some(code) => format!("the login exited with {code}"),
            None => "the login exited".to_owned(),
        }));
    }
    drop(stream);

    let restored = write_all(&mut stdout, RESET)
        .and_then(|()| execute!(stdout, EnterAlternateScreen))
        .and_then(|()| terminal.clear());
    match restored {
        Ok(()) => ended,
        Err(err) => Ended::Failed(format!("cannot restore the screen: {err}")),
    }
}

/// The end of a terminal's output: where herder's own last line about a login is.
#[derive(Debug, Default)]
pub struct Tail(Vec<u8>);

impl Tail {
    /// Bytes kept; herder's lines are short.
    const KEEP: usize = 1024;

    fn push(&mut self, data: &[u8]) {
        self.0.extend_from_slice(data);
        let excess = self.0.len().saturating_sub(Self::KEEP);
        self.0.drain(..excess);
    }

    /// The last line the daemon wrote about the login, without its `herder: ` mark.
    fn outcome(&self) -> Option<String> {
        String::from_utf8_lossy(&self.0)
            .lines()
            .rev()
            .find_map(|line| line.trim().strip_prefix("herder: ").map(str::to_owned))
    }
}

/// Pipes bytes both ways until a detach or the shell's exit, keeping the output's `tail`.
async fn pipe(
    stream: &TerminalStream,
    mut typed: mpsc::UnboundedReceiver<Vec<u8>>,
    stdout: &mut std::io::Stdout,
    tail: &mut Tail,
) -> Ended {
    use tokio::signal::unix::{SignalKind, signal};
    // Without the signal, resizes are just not forwarded.
    let mut winch = signal(SignalKind::window_change()).ok();
    let mut prefix = Prefix::default();
    loop {
        tokio::select! {
            event = stream.next() => match event {
                Some(TerminalEvent::Output { data }) => {
                    tail.push(&data);
                    if let Err(err) = write_all(stdout, &data) {
                        return Ended::Failed(format!("cannot write to the screen: {err}"));
                    }
                }
                Some(TerminalEvent::Reattached) => {
                    // The scrollback replays from the start: draw it on a clean screen.
                    let _ = write_all(stdout, RESET).and_then(|()| write_all(stdout, b"\x1b[H\x1b[2J"));
                }
                Some(TerminalEvent::Closed { exit_code }) => return Ended::Exited(exit_code),
                None => return Ended::Failed("the connection to the machine stopped".to_owned()),
            },
            Some(bytes) = typed.recv() => {
                let (send, detach) = prefix.feed(&bytes);
                if !send.is_empty() {
                    stream.input(send);
                }
                if detach {
                    return Ended::Detached;
                }
            }
            Some(()) = async {
                match winch.as_mut() {
                    Some(winch) => winch.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                if let Ok((cols, rows)) = ratatui::crossterm::terminal::size() {
                    stream.resize(cols, rows);
                }
            }
        }
    }
}

/// A failed open or attach, for the status line.
fn failure(err: &Error) -> String {
    match err {
        Error::Rejected { info } if info.code == ErrorCode::Forbidden => OWNER_ONLY.to_owned(),
        err => format!("cannot attach: {err}"),
    }
}

fn write_all(stdout: &mut std::io::Stdout, data: &[u8]) -> std::io::Result<()> {
    stdout.write_all(data)?;
    stdout.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_prefix_detaches_on_d_and_escapes_itself() {
        let mut prefix = Prefix::default();
        assert_eq!(prefix.feed(b"ls\r"), (b"ls\r".to_vec(), false));
        // Ctrl-] Ctrl-] sends one; Ctrl-] then another key sends both.
        assert_eq!(prefix.feed(&[PREFIX, PREFIX]), (vec![PREFIX], false));
        assert_eq!(prefix.feed(&[PREFIX, b'x']), (vec![PREFIX, b'x'], false));
        // The prefix may arrive in one read and the key in the next.
        assert_eq!(prefix.feed(b"a\x1d"), (b"a".to_vec(), false));
        assert_eq!(prefix.feed(b"d:q"), (Vec::new(), true));
        let mut prefix = Prefix::default();
        assert_eq!(prefix.feed(b"x\x1dD"), (b"x".to_vec(), true));
        // As a terminal with extended keys on sends it.
        for encoded in ENCODED_PREFIX {
            let mut prefix = Prefix::default();
            assert_eq!(
                prefix.feed(&[b"a", encoded, b"d"].concat()),
                (b"a".to_vec(), true)
            );
            let mut prefix = Prefix::default();
            let twice = [encoded, encoded].concat();
            assert_eq!(prefix.feed(&twice), (vec![PREFIX], false));
        }
    }

    #[test]
    fn a_login_ends_with_herders_last_line() {
        let mut tail = Tail::default();
        assert_eq!(tail.outcome(), None);
        tail.push(b"Open https://example.com\r\nherder: not this\r\n");
        tail.push(&[b'x'; 2000]);
        tail.push(b"\r\nherder: added account work\r\n");
        assert_eq!(tail.outcome().as_deref(), Some("added account work"));
        assert_eq!(tail.0.len(), Tail::KEEP);
    }

    #[test]
    fn endings_read_as_notices() {
        assert_eq!(Ended::Exited(Some(3)).notice(), "terminal exited with 3");
        assert_eq!(Ended::Exited(None).notice(), "terminal exited");
        assert_eq!(
            Ended::Login("added account work".into()).notice(),
            "added account work"
        );
        let forbidden = Error::Rejected {
            info: herder_protocol::ErrorInfo {
                code: ErrorCode::Forbidden,
                message: "owners only".into(),
            },
        };
        assert_eq!(failure(&forbidden), OWNER_ONLY);
    }
}

#[cfg(test)]
pub(crate) mod app_tests {
    use herder_protocol::{AccountId, Terminal};
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;
    use crate::app::{App, Effect, Msg};
    use crate::fake;

    fn press(app: &mut App, code: KeyCode) -> Vec<Effect> {
        app.update(Msg::Key(KeyEvent::new(code, KeyModifiers::NONE)))
    }

    /// [`fake::tree`] with two terminals open in `s2`, the first selected session, one in
    /// `s1`, and an account login, which no session's picker lists.
    pub(crate) fn with_terminals() -> App {
        let mut app = fake::tree();
        let mut machines = app.machines.clone();
        machines[0].terminals = ["t1", "t2", "t3"]
            .iter()
            .zip(["s2", "s1", "s2"])
            .map(|(terminal, session)| Terminal {
                terminal_id: TerminalId::new(*terminal),
                purpose: TerminalPurpose::Shell {
                    session_id: SessionId::new(session),
                },
            })
            .chain([Terminal {
                terminal_id: TerminalId::new("t4"),
                purpose: TerminalPurpose::Login {
                    account_id: AccountId::new("a1"),
                },
            }])
            .collect();
        app.update(Msg::Machines(machines));
        app
    }

    #[test]
    fn t_lists_the_sessions_terminals_and_enter_attaches() {
        let mut app = with_terminals();
        assert_eq!(press(&mut app, KeyCode::Char('t')), []);
        let picker = app.terminals.clone().unwrap();
        assert_eq!(picker.session, fake::key("h1", "s2"));
        assert_eq!(
            rows(&app.machines, &picker.session),
            [
                Target::New(SessionId::new("s2")),
                Target::Existing(TerminalId::new("t1")),
                Target::Existing(TerminalId::new("t3")),
            ]
        );
        // j/k move within the picker, not the session list.
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('k'));
        assert_eq!(
            app.selected(),
            Some(crate::app::Row::Session {
                key: fake::key("h1", "s2"),
                depth: 0,
            })
        );
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            [Effect::AttachTerminal {
                host_id: HostId::new("h1"),
                target: Target::Existing(TerminalId::new("t1")),
            }]
        );
        assert_eq!(app.terminals, None);

        // The first row opens a new shell; Esc closes without attaching.
        press(&mut app, KeyCode::Char('t'));
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            [Effect::AttachTerminal {
                host_id: HostId::new("h1"),
                target: Target::New(SessionId::new("s2")),
            }]
        );
        press(&mut app, KeyCode::Char('t'));
        assert_eq!(press(&mut app, KeyCode::Esc), []);
        assert_eq!(app.terminals, None);
    }

    #[test]
    fn a_member_is_told_terminals_are_owner_only() {
        let mut app = fake::tree();
        let mut machines = app.machines.clone();
        machines[0].role = Some(Role::Member);
        app.update(Msg::Machines(machines));
        press(&mut app, KeyCode::Char('t'));
        assert_eq!(app.terminals, None);
        assert_eq!(app.notice.as_deref(), Some(OWNER_ONLY));
        // The next key clears it.
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.notice, None);
    }

    #[test]
    fn the_end_of_an_attach_shows_in_the_status_line() {
        let mut app = fake::tree();
        app.update(Msg::TerminalEnded(Ended::Exited(Some(3))));
        assert_eq!(app.notice.as_deref(), Some("terminal exited with 3"));
    }
}
