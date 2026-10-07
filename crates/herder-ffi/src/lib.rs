//! UniFFI bindings of `herder-client-core`, for the Swift apps.
//!
//! The objects wrap their client-core counterparts one to one, and the records and enums are
//! the client-core and protocol types themselves, declared to UniFFI in [`types`]. `API.md`
//! of `herder-client-core` is the contract; this crate adds only what a foreign language
//! needs on top of it:
//!
//! - The async runtime. [`Client::open`] starts a tokio runtime that the client and its
//!   streams run on, whatever executor the foreign side polls their futures from. Dropping a
//!   pending future (cancelling a Swift `Task` or a Kotlin coroutine) aborts the call.
//! - [`HerderError`], client-core's `Error` under a name and with field names that do not
//!   clash with Swift's `Error` or Kotlin's `Throwable.message`.
//! - [`parse_pairing_uri`] and [`pairing_uri_to_string`], `PairingUri`'s `FromStr` and
//!   `Display`, and [`parse_pairing_link`] and [`pairing_link_to_string`], `PairingLink`'s,
//!   which UniFFI cannot export as trait impls on a record.
//! - [`image_media_types`], [`max_image_bytes`], [`max_file_bytes`], [`max_file_name_bytes`]
//!   and [`max_prompt_attachment_bytes`], the protocol's limits on a prompt's images and files,
//!   which UniFFI cannot export as constants.

mod types;

use std::future::Future;
use std::sync::Arc;

use herder_client_core as client_core;
use herder_client_core::{
    Machine, NewAccount, PairResult, PairingLink, PairingUri, SessionUpdate, SharedLink,
    TerminalEvent,
};
use herder_protocol::{
    AccountId, CommandBody, CommandResult, ErrorInfo, HostId, SessionId, TerminalId,
};
use tokio::runtime::{Handle, Runtime};
use tokio_util::task::AbortOnDropHandle;

uniffi::setup_scaffolding!();

/// Why a client call failed; client-core's `Error`.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error, uniffi::Error)]
pub enum HerderError {
    /// The pairing link is not a valid `herder://pair` link.
    #[error("{detail}")]
    InvalidLink {
        /// What is wrong with it.
        detail: String,
    },
    /// Pairing failed: no address answered, the certificate did not match, or the daemon
    /// refused the code.
    #[error("pairing failed: {detail}")]
    Pairing {
        /// Why.
        detail: String,
    },
    /// No paired machine has this host id.
    #[error("no paired machine {host_id}")]
    UnknownMachine {
        /// The host id asked for.
        host_id: HostId,
    },
    /// The daemon refused the command; nothing changed.
    #[error("{}", .info.message)]
    Rejected {
        /// The daemon's error; `info.code` says why.
        info: ErrorInfo,
    },
    /// No address of the machine answered.
    #[error("no address answered: {detail}")]
    Unreachable {
        /// Why each address did not answer.
        detail: String,
    },
    /// Something on this device failed: the profile file, a device key, or the runtime.
    #[error("{detail}")]
    Local {
        /// What failed.
        detail: String,
    },
    /// The client stopped before the call finished.
    #[error("the client stopped")]
    Closed,
}

impl From<client_core::Error> for HerderError {
    fn from(error: client_core::Error) -> Self {
        match error {
            client_core::Error::InvalidLink { message } => Self::InvalidLink { detail: message },
            client_core::Error::Pairing { message } => Self::Pairing { detail: message },
            client_core::Error::UnknownMachine { host_id } => Self::UnknownMachine { host_id },
            client_core::Error::Rejected { info } => Self::Rejected { info },
            client_core::Error::Unreachable { message } => Self::Unreachable { detail: message },
            client_core::Error::Local { message } => Self::Local { detail: message },
            client_core::Error::Closed => Self::Closed,
        }
    }
}

/// The version of client-core's API these bindings wrap, `CLIENT_API_VERSION`.
#[uniffi::export]
pub fn client_api_version() -> u32 {
    client_core::CLIENT_API_VERSION
}

/// Most turns a host may be set to run at once, `herder_protocol::MAX_TURNS_LIMIT`.
#[uniffi::export]
pub fn max_turns_limit() -> u32 {
    herder_protocol::MAX_TURNS_LIMIT
}

/// The media types a prompt's image may have, `herder_protocol::IMAGE_MEDIA_TYPES`.
#[uniffi::export]
pub fn image_media_types() -> Vec<String> {
    herder_protocol::IMAGE_MEDIA_TYPES
        .iter()
        .map(|media_type| (*media_type).to_owned())
        .collect()
}

/// The most bytes one image of a prompt may have, `herder_protocol::MAX_IMAGE_BYTES`.
#[uniffi::export]
pub fn max_image_bytes() -> u64 {
    herder_protocol::MAX_IMAGE_BYTES as u64
}

/// The most bytes one file of a prompt may have, `herder_protocol::MAX_FILE_BYTES`.
#[uniffi::export]
pub fn max_file_bytes() -> u64 {
    herder_protocol::MAX_FILE_BYTES as u64
}

/// The most bytes the name of a prompt's file may have, `herder_protocol::MAX_FILE_NAME_BYTES`.
#[uniffi::export]
pub fn max_file_name_bytes() -> u64 {
    herder_protocol::MAX_FILE_NAME_BYTES as u64
}

/// The most bytes all images and files of one prompt may have together,
/// `herder_protocol::MAX_PROMPT_ATTACHMENT_BYTES`.
#[uniffi::export]
pub fn max_prompt_attachment_bytes() -> u64 {
    herder_protocol::MAX_PROMPT_ATTACHMENT_BYTES as u64
}

/// The most bytes a project's icon may have, `herder_protocol::MAX_PROJECT_ICON_BYTES`.
#[uniffi::export]
pub fn max_project_icon_bytes() -> u64 {
    herder_protocol::MAX_PROJECT_ICON_BYTES as u64
}

/// Parses a `herder://pair` link, to confirm it before pairing.
#[uniffi::export]
pub fn parse_pairing_uri(link: String) -> Result<PairingUri, HerderError> {
    Ok(link.parse::<PairingUri>()?)
}

/// Formats a pairing link as `herder://pair?…`.
#[uniffi::export]
pub fn pairing_uri_to_string(uri: PairingUri) -> String {
    uri.to_string()
}

/// Parses a `herder://pair` link of one or more machines, to confirm it before pairing.
#[uniffi::export]
pub fn parse_pairing_link(link: String) -> Result<PairingLink, HerderError> {
    Ok(link.parse::<PairingLink>()?)
}

/// Formats a link of one or more machines as `herder://pair?…`, for a QR code.
#[uniffi::export]
pub fn pairing_link_to_string(link: PairingLink) -> String {
    link.to_string()
}

/// Runs `call` on the client's runtime and waits for it; `None` once the runtime is gone.
/// Dropping the returned future aborts the call.
async fn on<T: Send + 'static>(
    runtime: &Handle,
    call: impl Future<Output = T> + Send + 'static,
) -> Option<T> {
    match AbortOnDropHandle::new(runtime.spawn(call)).await {
        Ok(value) => Some(value),
        Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
        Err(_) => None,
    }
}

/// [`on`] for calls that fail with an error.
async fn call<T: Send + 'static>(
    runtime: &Handle,
    call: impl Future<Output = Result<T, client_core::Error>> + Send + 'static,
) -> Result<T, HerderError> {
    on(runtime, call)
        .await
        .ok_or(HerderError::Closed)?
        .map_err(HerderError::from)
}

/// The tokio runtime of one [`Client`], shut down without waiting when the client goes.
struct ClientRuntime(Option<Runtime>);

impl Drop for ClientRuntime {
    fn drop(&mut self) {
        // Unlike dropping it, this does not panic if a foreign callback releases the last
        // reference to the client from inside a runtime thread.
        if let Some(runtime) = self.0.take() {
            runtime.shutdown_background();
        }
    }
}

/// The client: paired machines and one connection supervisor per machine.
#[derive(uniffi::Object)]
pub struct Client {
    inner: client_core::Client,
    handle: Handle,
    // Declared last so the client stops before its runtime does.
    _runtime: ClientRuntime,
}

#[uniffi::export]
impl Client {
    /// Opens the profile in `config_dir` and starts connecting to every saved machine;
    /// `client` names the client in daemon logs.
    #[uniffi::constructor]
    pub fn open(config_dir: String, client: String) -> Result<Arc<Self>, HerderError> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("herder")
            .enable_all()
            .build()
            .map_err(|error| HerderError::Local {
                detail: format!("cannot start the async runtime: {error}"),
            })?;
        let inner = {
            let _entered = runtime.enter();
            client_core::Client::open(config_dir, client)?
        };
        Ok(Arc::new(Self {
            inner,
            handle: runtime.handle().clone(),
            _runtime: ClientRuntime(Some(runtime)),
        }))
    }

    /// Every paired machine, in pairing order.
    pub fn machines(&self) -> Vec<Machine> {
        self.inner.machines()
    }

    /// Notifications that `machines()` changed.
    pub fn changes(&self) -> Arc<Changes> {
        Arc::new(Changes {
            inner: Arc::new(self.inner.changes()),
            handle: self.handle.clone(),
        })
    }

    /// Pairs with every machine a `herder://pair` link names and saves each that paired;
    /// one result per machine, in the link's order.
    pub async fn pair(&self, link: String) -> Result<Vec<PairResult>, HerderError> {
        let client = self.inner.clone();
        call(&self.handle, async move { client.pair(link).await }).await
    }

    /// Makes a link that pairs another device with every connected machine, as this
    /// device's user with its role on each; machines that gave no code are skipped.
    pub async fn share(&self) -> Result<SharedLink, HerderError> {
        let client = self.inner.clone();
        call(&self.handle, async move { client.share().await }).await
    }

    /// Shows a machine as `name` on this device.
    pub fn rename(&self, host_id: HostId, name: String) -> Result<(), HerderError> {
        Ok(self.inner.rename(host_id, name)?)
    }

    /// Connects to a machine at `addresses`, in this order of preference, from now on.
    pub fn set_addresses(
        &self,
        host_id: HostId,
        addresses: Vec<String>,
    ) -> Result<(), HerderError> {
        Ok(self.inner.set_addresses(host_id, addresses)?)
    }

    /// Drops a machine's connection and connects again at once, racing its addresses in
    /// order: the address the new connection uses.
    pub async fn reconnect(&self, host_id: HostId) -> Result<String, HerderError> {
        let client = self.inner.clone();
        call(&self.handle, async move { client.reconnect(host_id).await }).await
    }

    /// Unpairs a machine on this device.
    pub fn forget(&self, host_id: HostId) -> Result<(), HerderError> {
        Ok(self.inner.forget(host_id)?)
    }

    /// Waits until a machine is connected and has sent everything owed for what was sent
    /// before.
    pub async fn synced(&self, host_id: HostId) -> Result<(), HerderError> {
        let client = self.inner.clone();
        call(&self.handle, async move { client.synced(host_id).await }).await
    }

    /// The app went to the background: saves the offline cache, blocking on the file system,
    /// and stops retrying lost connections until `wake()`.
    pub fn suspend(&self) {
        self.inner.suspend();
    }

    /// The app is in the foreground: reconnects every disconnected machine now and probes
    /// every connected one, replacing a dead connection.
    pub fn wake(&self) {
        self.inner.wake();
    }

    /// Streams a session, cached state first, across reconnects.
    pub fn subscribe_session(
        &self,
        host_id: HostId,
        session_id: SessionId,
    ) -> Result<Arc<SessionSubscription>, HerderError> {
        Ok(Arc::new(SessionSubscription {
            inner: Arc::new(self.inner.subscribe_session(host_id, session_id)?),
            handle: self.handle.clone(),
        }))
    }

    /// Sends a command and waits for the answer; resent with the same id after a reconnect.
    pub async fn send(
        &self,
        host_id: HostId,
        command: CommandBody,
    ) -> Result<CommandResult, HerderError> {
        let client = self.inner.clone();
        call(
            &self.handle,
            async move { client.send(host_id, command).await },
        )
        .await
    }

    /// Forks a session `source` lists onto `destination`: from its own journal there, else
    /// relayed from the session's machine while it is connected, else from the destination's
    /// vault; owners of `destination` only. Answers `session_forked`.
    pub async fn fork_session(
        &self,
        source: HostId,
        session_id: SessionId,
        destination: HostId,
        account_id: Option<AccountId>,
    ) -> Result<CommandResult, HerderError> {
        let client = self.inner.clone();
        call(&self.handle, async move {
            client
                .fork_session(source, session_id, destination, account_id)
                .await
        })
        .await
    }

    /// Opens a shell in a session's worktree; owners only.
    pub async fn open_terminal(
        &self,
        host_id: HostId,
        session_id: SessionId,
        cols: u16,
        rows: u16,
    ) -> Result<Arc<TerminalStream>, HerderError> {
        let client = self.inner.clone();
        let stream = call(&self.handle, async move {
            client.open_terminal(host_id, session_id, cols, rows).await
        })
        .await?;
        Ok(self.terminal(stream))
    }

    /// Runs a provider login in a login terminal; owners only.
    pub async fn add_account(
        &self,
        host_id: HostId,
        account: NewAccount,
        cols: u16,
        rows: u16,
    ) -> Result<Arc<TerminalStream>, HerderError> {
        let client = self.inner.clone();
        let stream = call(&self.handle, async move {
            client.add_account(host_id, account, cols, rows).await
        })
        .await?;
        Ok(self.terminal(stream))
    }

    /// Logs an existing account in again in a login terminal; owners only.
    pub async fn log_in_account(
        &self,
        host_id: HostId,
        account_id: AccountId,
        cols: u16,
        rows: u16,
    ) -> Result<Arc<TerminalStream>, HerderError> {
        let client = self.inner.clone();
        let stream = call(&self.handle, async move {
            client.log_in_account(host_id, account_id, cols, rows).await
        })
        .await?;
        Ok(self.terminal(stream))
    }

    /// Attaches to an open terminal; owners only, one stream per terminal per client.
    pub async fn attach_terminal(
        &self,
        host_id: HostId,
        terminal_id: TerminalId,
    ) -> Result<Arc<TerminalStream>, HerderError> {
        let client = self.inner.clone();
        let stream = call(&self.handle, async move {
            client.attach_terminal(host_id, terminal_id).await
        })
        .await?;
        Ok(self.terminal(stream))
    }
}

impl Client {
    fn terminal(&self, stream: client_core::TerminalStream) -> Arc<TerminalStream> {
        Arc::new(TerminalStream {
            inner: Arc::new(stream),
            handle: self.handle.clone(),
        })
    }
}

/// Notifications that a client's machines changed.
#[derive(uniffi::Object)]
pub struct Changes {
    inner: Arc<client_core::Changes>,
    handle: Handle,
}

#[uniffi::export]
impl Changes {
    /// `true` once the machines changed, coalescing; `false` once the client stops.
    pub async fn next(&self) -> bool {
        let changes = Arc::clone(&self.inner);
        on(&self.handle, async move { changes.next().await })
            .await
            .unwrap_or(false)
    }
}

/// A session's updates; releasing it unsubscribes.
#[derive(uniffi::Object)]
pub struct SessionSubscription {
    inner: Arc<client_core::SessionSubscription>,
    handle: Handle,
}

#[uniffi::export]
impl SessionSubscription {
    /// The next update; `None` once the client or the machine stops.
    pub async fn next(&self) -> Option<SessionUpdate> {
        let subscription = Arc::clone(&self.inner);
        on(&self.handle, async move { subscription.next().await })
            .await
            .flatten()
    }
}

/// A terminal's output and input; releasing it detaches, and the shell keeps running.
#[derive(uniffi::Object)]
pub struct TerminalStream {
    inner: Arc<client_core::TerminalStream>,
    handle: Handle,
}

#[uniffi::export]
impl TerminalStream {
    /// The terminal.
    pub fn terminal_id(&self) -> TerminalId {
        self.inner.terminal_id()
    }

    /// The next output, re-attach or exit; `None` once the client stops.
    pub async fn next(&self) -> Option<TerminalEvent> {
        let stream = Arc::clone(&self.inner);
        on(&self.handle, async move { stream.next().await })
            .await
            .flatten()
    }

    /// Writes bytes to the terminal's input; dropped while disconnected.
    pub fn input(&self, data: Vec<u8>) {
        self.inner.input(data);
    }

    /// Changes the terminal's size.
    pub fn resize(&self, cols: u16, rows: u16) {
        self.inner.resize(cols, rows);
    }
}
