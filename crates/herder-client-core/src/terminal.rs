//! Terminal streams: what one attached terminal sends this client, across reconnects.

use std::sync::Arc;

use herder_protocol::{CommandBody, TerminalId};
use tokio::sync::mpsc;

use crate::supervisor::Supervisor;

/// Something an attached terminal did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalEvent {
    /// Bytes the terminal wrote, to show as they are. The first output after attaching is the
    /// daemon's scrollback.
    Output {
        /// The bytes, raw: not necessarily valid UTF-8, nor split at character boundaries.
        data: Vec<u8>,
    },
    /// The connection was lost and the terminal re-attached on a new one: the daemon now
    /// replays its whole scrollback, so whatever was drawn from earlier output is to be
    /// discarded (for a local terminal: reset it) before the next output.
    Reattached,
    /// The shell exited; the stream ends. `exit_code` is absent when a signal ended the shell,
    /// or when it exited while this client was disconnected.
    Closed {
        /// The shell's exit status.
        exit_code: Option<i32>,
    },
}

/// One attached terminal: its events, and its input. Dropping it detaches; the shell keeps
/// running.
pub struct TerminalStream {
    pub(crate) supervisor: Arc<Supervisor>,
    pub(crate) terminal_id: TerminalId,
    pub(crate) events: tokio::sync::Mutex<mpsc::UnboundedReceiver<TerminalEvent>>,
}

impl TerminalStream {
    /// The terminal.
    pub fn terminal_id(&self) -> TerminalId {
        self.terminal_id.clone()
    }

    /// The next event, waiting for one; `None` after [`TerminalEvent::Closed`], or once the
    /// client or the machine stops.
    pub async fn next(&self) -> Option<TerminalEvent> {
        let mut events = self.events.lock().await;
        tokio::select! {
            () = self.supervisor.stop.cancelled() => None,
            event = events.recv() => event,
        }
    }

    /// Writes `data` to the terminal's input. Best effort: while the machine is disconnected
    /// the bytes are dropped, as typing into a dead line would be.
    pub fn input(&self, data: Vec<u8>) {
        self.supervisor.terminal_once(
            &self.terminal_id,
            CommandBody::TerminalInput {
                terminal_id: self.terminal_id.clone(),
                data: herder_protocol::Bytes(data),
            },
        );
    }

    /// Resizes the terminal. Sent at once when connected; the latest size is sent again after
    /// every re-attach, so a resize made while disconnected still lands.
    pub fn resize(&self, cols: u16, rows: u16) {
        self.supervisor
            .terminal_resize(&self.terminal_id, cols, rows);
    }
}

impl Drop for TerminalStream {
    fn drop(&mut self) {
        self.supervisor.terminal_dropped(&self.terminal_id);
    }
}
