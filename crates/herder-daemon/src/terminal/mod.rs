//! Terminals: shells on a pseudo-terminal in a session's worktree, for the daemon's owners.
//!
//! A terminal runs the user's login shell (`$SHELL -l`, or `/bin/sh -l`) with
//! `TERM=xterm-256color`. It outlives the clients that use it: detaching, or disconnecting,
//! leaves the shell running. Everything it writes is kept in a scrollback ring of the last
//! [`SCROLLBACK`] bytes; attaching replays the scrollback as one `terminal_output` message,
//! then streams live output. The bytes are ephemeral: never journaled, and gone once the
//! terminal closes.
//!
//! Any attached client may type into a terminal or resize it; the latest resize wins. A client
//! more than [`TERMINAL_BACKLOG`](crate::hub::TERMINAL_BACKLOG) messages behind is disconnected
//! rather than buffered without bound; it reconnects and re-attaches to get the scrollback.
//!
//! A terminal closes when its shell exits, or when its session is archived ([`KillOnArchive`])
//! or the daemon stops ([`Terminals::close_all`]), which hang the shell up. The owners'
//! terminal list ([`Hub::terminals_changed`]) is updated on every open and close; a close is
//! announced first with the shell's exit code ([`Hub::terminal_closed`]).
//!
//! Owner-only access is enforced before commands get here, by [`crate::auth::authorize`].

use std::collections::{BTreeMap, VecDeque};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use herder_protocol::{
    Account, Bytes, ErrorCode, ErrorInfo, Event, EventBody, Item, ItemId, ServerMessage,
    SessionHead, SessionId, SessionStatus, Terminal, TerminalId,
};
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use tracing::{debug, info, warn};

use crate::hub::{Hub, Outbox};
use crate::session::EventSink;

/// Bytes of output each terminal keeps for clients that attach later.
pub const SCROLLBACK: usize = 1024 * 1024;

/// Largest read from a terminal, and so the largest live `terminal_output` message.
const READ_CHUNK: usize = 16 * 1024;

/// The shell terminals run: `$SHELL`, or `/bin/sh` when it is unset.
pub fn login_shell() -> PathBuf {
    std::env::var_os("SHELL")
        .filter(|shell| !shell.is_empty())
        .map_or_else(|| PathBuf::from("/bin/sh"), PathBuf::from)
}

/// Every open terminal on this daemon. Cheap to clone.
#[derive(Clone)]
pub struct Terminals {
    inner: Arc<Inner>,
}

struct Inner {
    hub: Arc<Hub>,
    shell: PathBuf,
    /// Open terminals, ordered by id, which is a ULID: oldest first.
    open: Mutex<BTreeMap<TerminalId, Arc<Term>>>,
}

/// One open terminal.
struct Term {
    session_id: SessionId,
    /// The pty's controlling side, for resizing.
    master: Mutex<Box<dyn MasterPty + Send>>,
    /// The shell's input.
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    /// Hangs the shell up.
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    output: Mutex<Output>,
}

#[derive(Default)]
struct Output {
    scrollback: VecDeque<u8>,
    /// Clients streaming the output.
    attached: Vec<Arc<Outbox>>,
}

impl Terminals {
    /// No terminals yet; new ones run `shell` and are announced through `hub`.
    pub fn new(hub: Arc<Hub>, shell: PathBuf) -> Self {
        Self {
            inner: Arc::new(Inner {
                hub,
                shell,
                open: Mutex::new(BTreeMap::new()),
            }),
        }
    }

    /// Every open terminal, oldest first.
    pub fn list(&self) -> Vec<Terminal> {
        list(&self.inner.lock())
    }

    /// Opens a shell of `cols` by `rows` in `cwd`, the worktree of `session_id`, and attaches
    /// `outbox`. The new terminal list is queued before any of its output.
    pub(crate) fn open(
        &self,
        session_id: SessionId,
        cwd: &Path,
        cols: u16,
        rows: u16,
        outbox: &Arc<Outbox>,
    ) -> Result<TerminalId, ErrorInfo> {
        let size = size(cols, rows)?;
        let pair = native_pty_system()
            .openpty(size)
            .map_err(|err| internal("cannot open a pseudo-terminal", &err))?;
        let mut command = CommandBuilder::new(&self.inner.shell);
        command.arg("-l");
        command.cwd(cwd);
        command.env("TERM", "xterm-256color");
        let child = pair
            .slave
            .spawn_command(command)
            .map_err(|err| internal("cannot start a shell", &err))?;
        // The shell holds the only other handle to its side, so reads end once it exits.
        drop(pair.slave);
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|err| internal("cannot read the pseudo-terminal", &err))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|err| internal("cannot write to the pseudo-terminal", &err))?;
        let terminal_id = TerminalId::new(ulid::Ulid::new().to_string());
        let term = Arc::new(Term {
            session_id: session_id.clone(),
            master: Mutex::new(pair.master),
            writer: Arc::new(Mutex::new(writer)),
            killer: Mutex::new(child.clone_killer()),
            output: Mutex::new(Output::default()),
        });
        {
            let mut open = self.inner.lock();
            open.insert(terminal_id.clone(), Arc::clone(&term));
            self.inner.hub.terminals_changed(&list(&open));
        }
        lock(&term.output).attached.push(Arc::clone(outbox));
        info!(%terminal_id, %session_id, "terminal opened");
        let terminals = self.clone();
        let id = terminal_id.clone();
        let spawned = std::thread::Builder::new()
            .name(format!("terminal-{terminal_id}"))
            .spawn(move || terminals.pump(&id, &term, reader, child));
        if let Err(err) = spawned {
            if let Some(term) = self.inner.remove(&terminal_id) {
                term.hang_up();
            }
            return Err(internal("cannot start a terminal reader", &err));
        }
        Ok(terminal_id)
    }

    /// Streams the terminal to `outbox`, starting with its scrollback. Attaching twice is a
    /// no-op.
    pub(crate) fn attach(
        &self,
        terminal_id: &TerminalId,
        outbox: &Arc<Outbox>,
    ) -> Result<(), ErrorInfo> {
        let term = self.get(terminal_id)?;
        let mut output = lock(&term.output);
        if output.is_attached(outbox) {
            return Ok(());
        }
        if !output.scrollback.is_empty() {
            let data = output.scrollback.iter().copied().collect();
            outbox.push(ServerMessage::TerminalOutput {
                terminal_id: terminal_id.clone(),
                data: Bytes(data),
            });
        }
        output.attached.push(Arc::clone(outbox));
        Ok(())
    }

    /// Stops streaming the terminal to `outbox`; the shell keeps running.
    pub(crate) fn detach(
        &self,
        terminal_id: &TerminalId,
        outbox: &Arc<Outbox>,
    ) -> Result<(), ErrorInfo> {
        let term = self.get(terminal_id)?;
        lock(&term.output)
            .attached
            .retain(|other| !Arc::ptr_eq(other, outbox));
        Ok(())
    }

    /// Resizes the terminal for a client attached to it.
    pub(crate) fn resize(
        &self,
        terminal_id: &TerminalId,
        outbox: &Arc<Outbox>,
        cols: u16,
        rows: u16,
    ) -> Result<(), ErrorInfo> {
        let size = size(cols, rows)?;
        let term = self.attached(terminal_id, outbox)?;
        lock(&term.master)
            .resize(size)
            .map_err(|err| internal("cannot resize the terminal", &err))
    }

    /// Writes `data` to the shell's input for a client attached to it.
    pub(crate) async fn input(
        &self,
        terminal_id: &TerminalId,
        outbox: &Arc<Outbox>,
        data: Vec<u8>,
    ) -> Result<(), ErrorInfo> {
        let writer = Arc::clone(&self.attached(terminal_id, outbox)?.writer);
        // A shell that is not reading fills the pty's buffer and blocks the write.
        tokio::task::spawn_blocking(move || {
            let mut writer = lock(&writer);
            writer.write_all(&data).and_then(|()| writer.flush())
        })
        .await
        .map_err(|err| internal("cannot write to the terminal", &err))?
        .map_err(|err| internal("cannot write to the terminal", &err))
    }

    /// Detaches `outbox` from every terminal, as its connection closed.
    pub(crate) fn disconnect(&self, outbox: &Arc<Outbox>) {
        for term in self.inner.lock().values() {
            lock(&term.output)
                .attached
                .retain(|other| !Arc::ptr_eq(other, outbox));
        }
    }

    /// Hangs up every terminal of `session_id`; each closes once its shell exits.
    pub fn close_session(&self, session_id: &SessionId) {
        for (terminal_id, term) in self.inner.lock().iter() {
            if term.session_id == *session_id {
                info!(%terminal_id, %session_id, "closing the terminal of an archived session");
                term.hang_up();
            }
        }
    }

    /// Hangs up every terminal, as the daemon stops.
    pub fn close_all(&self) {
        for term in self.inner.lock().values() {
            term.hang_up();
        }
    }

    /// Reads the shell's output until it exits, then closes the terminal. Runs on its own
    /// thread: the reads block.
    fn pump(
        &self,
        terminal_id: &TerminalId,
        term: &Term,
        mut reader: Box<dyn Read + Send>,
        mut child: Box<dyn portable_pty::Child + Send + Sync>,
    ) {
        let mut buf = vec![0; READ_CHUNK];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => term.publish(terminal_id, &buf[..n]),
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
                // Linux reports EIO once the last process holding the shell's side exits.
                Err(err) => {
                    debug!(%terminal_id, "terminal read ended: {err}");
                    break;
                }
            }
        }
        let exit_code = match child.wait() {
            Ok(status) => {
                info!(
                    %terminal_id,
                    session_id = %term.session_id,
                    exit_code = status.exit_code(),
                    signal = status.signal(),
                    "terminal closed"
                );
                // portable-pty reports a signal death as exit code 1 with the signal's name.
                match status.signal() {
                    Some(_) => None,
                    None => i32::try_from(status.exit_code()).ok(),
                }
            }
            Err(err) => {
                warn!(%terminal_id, "cannot wait for the terminal's shell: {err}");
                None
            }
        };
        self.inner.close(terminal_id, exit_code);
    }

    fn get(&self, terminal_id: &TerminalId) -> Result<Arc<Term>, ErrorInfo> {
        self.inner
            .lock()
            .get(terminal_id)
            .cloned()
            .ok_or_else(|| ErrorInfo {
                code: ErrorCode::NotFound,
                message: format!("terminal {terminal_id} does not exist"),
            })
    }

    /// The terminal, if `outbox` is attached to it.
    fn attached(
        &self,
        terminal_id: &TerminalId,
        outbox: &Arc<Outbox>,
    ) -> Result<Arc<Term>, ErrorInfo> {
        let term = self.get(terminal_id)?;
        if !lock(&term.output).is_attached(outbox) {
            return Err(ErrorInfo {
                code: ErrorCode::Conflict,
                message: format!("attach to terminal {terminal_id} first"),
            });
        }
        Ok(term)
    }
}

impl Inner {
    /// Forgets a terminal whose shell exited, and announces it with the new list.
    fn close(&self, terminal_id: &TerminalId, exit_code: Option<i32>) {
        let mut open = self.lock();
        if open.remove(terminal_id).is_some() {
            self.hub
                .terminal_closed(terminal_id, exit_code, &list(&open));
        }
    }

    /// Forgets a terminal and announces the new list.
    fn remove(&self, terminal_id: &TerminalId) -> Option<Arc<Term>> {
        let mut open = self.lock();
        let term = open.remove(terminal_id)?;
        self.hub.terminals_changed(&list(&open));
        Some(term)
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<TerminalId, Arc<Term>>> {
        lock(&self.open)
    }
}

impl Term {
    /// Adds output to the scrollback and sends it to every attached client, letting go of
    /// clients that fell too far behind.
    fn publish(&self, terminal_id: &TerminalId, data: &[u8]) {
        let mut output = lock(&self.output);
        keep(&mut output.scrollback, data);
        output.attached.retain(|outbox| {
            outbox.terminal_output(ServerMessage::TerminalOutput {
                terminal_id: terminal_id.clone(),
                data: Bytes(data.to_vec()),
            })
        });
    }

    fn hang_up(&self) {
        if let Err(err) = lock(&self.killer).kill() {
            debug!("cannot hang up a terminal: {err}");
        }
    }
}

impl Output {
    fn is_attached(&self, outbox: &Arc<Outbox>) -> bool {
        self.attached.iter().any(|other| Arc::ptr_eq(other, outbox))
    }
}

/// Forwards session events to `next`, closing a session's terminals once it is archived: its
/// worktree is gone.
pub struct KillOnArchive {
    /// Where every event goes on to.
    pub next: Arc<dyn EventSink>,
    /// The terminals to close.
    pub terminals: Terminals,
}

impl EventSink for KillOnArchive {
    fn event(&self, event: &Event) {
        if let EventBody::SessionStatusChanged {
            status: SessionStatus::Archived,
        } = event.body
        {
            self.terminals.close_session(&event.session_id);
        }
        self.next.event(event);
    }

    fn snapshot(&self, session_id: &SessionId, item: &Item) {
        self.next.snapshot(session_id, item);
    }

    fn delta(&self, session_id: &SessionId, item_id: &ItemId, text: &str) {
        self.next.delta(session_id, item_id, text);
    }

    fn sessions_changed(&self, sessions: &[SessionHead]) {
        self.next.sessions_changed(sessions);
    }

    fn accounts_changed(&self, accounts: &[Account]) {
        self.next.accounts_changed(accounts);
    }
}

/// Appends `data` to a scrollback ring, dropping the oldest bytes past [`SCROLLBACK`].
fn keep(scrollback: &mut VecDeque<u8>, data: &[u8]) {
    scrollback.extend(data);
    if let Some(excess) = scrollback.len().checked_sub(SCROLLBACK) {
        scrollback.drain(..excess);
    }
}

fn list(open: &BTreeMap<TerminalId, Arc<Term>>) -> Vec<Terminal> {
    open.iter()
        .map(|(terminal_id, term)| Terminal {
            terminal_id: terminal_id.clone(),
            session_id: term.session_id.clone(),
        })
        .collect()
}

fn size(cols: u16, rows: u16) -> Result<PtySize, ErrorInfo> {
    if cols == 0 || rows == 0 {
        return Err(ErrorInfo {
            code: ErrorCode::BadRequest,
            message: "a terminal is at least 1 column by 1 row".to_owned(),
        });
    }
    Ok(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    })
}

fn internal(context: &str, err: &dyn std::fmt::Display) -> ErrorInfo {
    warn!("{context}: {err:#}");
    ErrorInfo {
        code: ErrorCode::Internal,
        message: format!("{context}: {err:#}"),
    }
}

fn lock<T: ?Sized>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // Every update is a single insert, remove, retain or write, so a poisoned lock is still
    // consistent.
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests;
