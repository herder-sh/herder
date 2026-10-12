//! Forks: a session's history copied into a new session that goes on here.
//!
//! The session to fork is found on this host, else in the history a client relayed from its
//! host ([`SessionManager::upload_history`]), else in the vault this host replicates to
//! ([`crate::vault::fork`]), wherever it runs and whether its host is up or gone. A relayed
//! history is taken as it is, without asking the vault: hosts do not talk to each other, but
//! the client is connected to both, reads the session from its host and uploads it here, in
//! parts that each fit a WebSocket message. Only owners may upload one, and its events are
//! imported as history, as a vault copy's are. The original
//! is left as it is. The fork gets a new id; its journal is the original's with that id, minus
//! the pull requests and other branches the original tracked, so it owns only its own branch.
//! Its `session_created` names this host's clone of the repository, its new worktree and its
//! new branch. The worktree is restored from the original's latest checkpoint: a local one
//! when the original ran here, else one on `origin` ([`checkpoint::fetch_latest`]). A
//! read-only original's fork is idle, and the fork moves to an account of this host by
//! `account_switched` when the original's is not here. Its journal marks the fork with
//! `session_forked`, naming the original and its host, and both that and the account switch
//! are `by` the user who forked it; its next prompt starts the CLI seeded with the transcript,
//! as after any switch. The images and files its prompts carried are kept for it under their ids, so
//! `get_attachment` answers as for the original.
//!
//! A turn the original had open is failed on the fork, its open approvals expired, as after a
//! restart, but saying it was handed off. When a user's prompt opened it, that prompt, with its
//! images and files, runs again on the fork right away, as failover retries a turn: queued `by` the
//! same user, it starts the CLI seeded with the transcript, which holds the failed turn's
//! partial items. A turn opened otherwise, by an agent's message or a follow-up,
//! which belong to the host they were sent to, leaves the fork `needs_you`. The original is not
//! told: its client interrupts it.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use herder_protocol::{
    AccountId, Attachment, Bytes, ErrorCode, ErrorInfo, Event, EventBody, HistoryPart, HostId,
    ItemBody, ProjectId, Provider, Relay, SessionId, SessionStatus, UserId,
};
use herder_store::QueuedPrompt;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use super::{SessionManager, actor, attachments, error, internal, worktree_error};
use crate::vault::fork::FromVault;
use crate::worktree::{self, checkpoint};

/// What `fork_session` and `herder fork` ask for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    /// The session to fork.
    pub session_id: SessionId,
    /// The account the fork runs on; picked when absent.
    #[serde(default)]
    pub account_id: Option<AccountId>,
}

/// A fork made here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Forked {
    /// The new session.
    pub session_id: SessionId,
    /// The account it runs on.
    pub account_id: AccountId,
    /// The session it was forked from.
    pub forked_from: SessionId,
    /// The host that session ran on.
    pub from_host_id: HostId,
    /// Its worktree.
    pub worktree: String,
    /// The branch checked out there; `None` when the fork works in the folder itself.
    pub branch: Option<String>,
    /// The checkpoint ref the worktree was restored from; `None` when there was none, so the
    /// worktree starts at the default branch.
    pub checkpoint: Option<String>,
}

/// Where forks find sessions: this host, and the vault it replicates to, if any.
pub struct Forks {
    /// This host.
    pub host: HostId,
    /// The vault, for sessions of other hosts.
    pub vault: Option<FromVault>,
}

/// A session to fork, as its host or the vault holds it.
pub struct Source {
    /// Its whole journal.
    pub events: Vec<Event>,
    /// The images and files its prompts carried, with their bytes.
    pub attachments: Vec<(Attachment, Bytes)>,
    /// Its project, whose clone here the fork works in; `None` for a session of this host,
    /// whose fork works in the same repository.
    pub project_id: Option<ProjectId>,
    /// The host it runs on.
    pub host_id: HostId,
}

/// How long an upload waits for its fork after its last part.
const UPLOAD_TTL: Duration = Duration::from_secs(10 * 60);

/// Histories being uploaded for a fork, by the user uploading and the session.
#[derive(Default)]
pub(super) struct Uploads(Mutex<HashMap<(UserId, SessionId), Upload>>);

struct Upload {
    events: Vec<Event>,
    attachments: Vec<(Attachment, Bytes)>,
    touched: Instant,
}

impl Uploads {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<(UserId, SessionId), Upload>> {
        // Every update is a single insert, push or removal, so a poisoned map is consistent.
        let mut uploads = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        uploads.retain(|_, upload| upload.touched.elapsed() < UPLOAD_TTL);
        uploads
    }
}

impl SessionManager {
    /// Adds `part` to the history `by` uploads of `session_id` for a relayed fork. Events
    /// starting at seq 1 start the upload over; anything else must follow them.
    pub async fn upload_history(
        &self,
        by: UserId,
        session_id: SessionId,
        part: HistoryPart,
    ) -> Result<(), ErrorInfo> {
        let mut uploads = self.inner.uploads.lock();
        let key = (by, session_id);
        if let HistoryPart::Events { events } = &part
            && events.first().is_some_and(|event| event.seq == 1)
        {
            uploads.remove(&key);
            uploads.insert(
                key.clone(),
                Upload {
                    events: Vec::new(),
                    attachments: Vec::new(),
                    touched: Instant::now(),
                },
            );
        }
        let Some(upload) = uploads.get_mut(&key) else {
            return Err(error(
                ErrorCode::BadRequest,
                format!("upload the history of {} from its first event", key.1),
            ));
        };
        upload.touched = Instant::now();
        match part {
            HistoryPart::Events { events } => upload.events.extend(events),
            HistoryPart::Image {
                attachment_id,
                image,
            } => {
                let attachment = Attachment {
                    attachment_id,
                    media_type: image.media_type,
                    size: image.data.0.len() as u64,
                    name: None,
                };
                upload.attachments.push((attachment, image.data));
            }
            HistoryPart::File { attachment, data } => upload.attachments.push((attachment, data)),
        }
        Ok(())
    }

    /// Lets `fork_session` and `herder fork` fork sessions of this host, and of other hosts
    /// through `forks.vault`. Replaces the source when the vault link changes. Without it,
    /// forks are refused.
    pub fn fork_from(&self, forks: Forks) -> anyhow::Result<()> {
        *self
            .inner
            .forks
            .write()
            .map_err(|_| anyhow::anyhow!("fork setup lock poisoned"))? = Some(Arc::new(forks));
        Ok(())
    }

    /// Forks `request.session_id` onto this host, `by` the user who asked; `None` when nobody
    /// has paired with this host yet, as when `herder fork` recovers a session onto a new one.
    /// With `relay`, a session not on this host is the history `by` uploaded from that host.
    pub async fn fork(
        &self,
        request: Request,
        relay: Option<Relay>,
        by: Option<UserId>,
    ) -> Result<Forked, ErrorInfo> {
        let forks = self
            .inner
            .forks
            .read()
            .map_err(|_| error(ErrorCode::Internal, "fork setup lock poisoned"))?
            .clone();
        let Some(forks) = forks else {
            return Err(error(
                ErrorCode::Unsupported,
                "this daemon does not fork sessions",
            ));
        };
        let session_id = &request.session_id;
        let local = self
            .inner
            .journal
            .session(session_id.clone())
            .await
            .map_err(internal)?
            .is_some();
        let source = if local {
            self.local_source(session_id, &forks.host).await?
        } else if let Some(relay) = relay {
            self.relayed_source(session_id, relay, by.as_ref(), &forks.host)?
        } else if let Some(vault) = &forks.vault {
            vault.source(session_id).await?
        } else {
            return Err(error(
                ErrorCode::NotFound,
                format!(
                    "session {session_id} is not on this host, which has no [vault] to find it in"
                ),
            ));
        };
        self.fork_source(source, request.account_id, by).await
    }

    /// The history `by` uploaded of `session_id`, from `relay.host_id`, to fork. It is checked
    /// as a vault copy is, by [`Self::fork_source`].
    fn relayed_source(
        &self,
        session_id: &SessionId,
        relay: Relay,
        by: Option<&UserId>,
        host: &HostId,
    ) -> Result<Source, ErrorInfo> {
        if relay.host_id == *host {
            return Err(error(
                ErrorCode::NotFound,
                format!("session {session_id} is not on this host"),
            ));
        }
        let upload = by.and_then(|by| {
            self.inner
                .uploads
                .lock()
                .remove(&(by.clone(), session_id.clone()))
        });
        let Some(upload) = upload else {
            return Err(error(
                ErrorCode::BadRequest,
                format!("no history of {session_id} was uploaded to fork"),
            ));
        };
        if upload
            .events
            .first()
            .is_some_and(|event| event.session_id != *session_id)
        {
            return Err(error(
                ErrorCode::BadRequest,
                format!("the uploaded history is not of {session_id}"),
            ));
        }
        Ok(Source {
            events: upload.events,
            attachments: upload.attachments,
            project_id: Some(relay.project_id),
            host_id: relay.host_id,
        })
    }

    /// A session of this host, to fork.
    async fn local_source(
        &self,
        session_id: &SessionId,
        host: &HostId,
    ) -> Result<Source, ErrorInfo> {
        let events = self
            .read_since(session_id, 0, usize::MAX)
            .await
            .map_err(internal)?;
        let mut kept = Vec::new();
        for event in &events {
            for attachment in event.body.attachments() {
                let id = &attachment.attachment_id;
                let (_, data) = attachments::fetch(&self.inner.attachments, session_id, id).await?;
                kept.push((attachment.clone(), data));
            }
        }
        Ok(Source {
            events,
            attachments: kept,
            project_id: None,
            host_id: host.clone(),
        })
    }

    /// Makes a new session here out of `source`, running on `account_id` or else on the
    /// source's account, its project's default account or this host's first account of its
    /// provider. The fork, and its move to that account, are `by` that user.
    async fn fork_source(
        &self,
        source: Source,
        account_id: Option<AccountId>,
        by: Option<UserId>,
    ) -> Result<Forked, ErrorInfo> {
        let inner = &self.inner;
        let Some(first) = source.events.first() else {
            return Err(error(ErrorCode::BadRequest, "the journal is empty"));
        };
        let original = first.session_id.clone();
        let EventBody::SessionCreated {
            repo: created_repo,
            parent,
            provider: created_on,
            account_id: created_account,
            chat,
            ..
        } = &first.body
        else {
            return Err(error(
                ErrorCode::BadRequest,
                format!("the journal of {original} does not start with its creation"),
            ));
        };
        if !source
            .events
            .iter()
            .map(|event| event.seq)
            .eq(1..=source.events.len() as u64)
            || source
                .events
                .iter()
                .any(|event| event.session_id != original)
        {
            return Err(error(
                ErrorCode::BadRequest,
                format!(
                    "the journal of {original} has gaps; this herder may be older than the host \
                     that recorded it"
                ),
            ));
        }
        if parent.is_some() {
            return Err(error(
                ErrorCode::Unsupported,
                format!("{original} is a child of a task; a task's child cannot be forked"),
            ));
        }
        if *chat {
            return Err(error(
                ErrorCode::Unsupported,
                format!("{original} is a chat; a chat cannot be forked"),
            ));
        }
        // Where the original stands at the end of its journal.
        let mut provider = created_on.clone();
        let mut current = created_account.clone();
        let mut status = SessionStatus::Idle;
        for event in &source.events {
            match &event.body {
                EventBody::AccountSwitched { account_id } => current = account_id.clone(),
                EventBody::ProviderSwitched {
                    provider: to,
                    account_id,
                    ..
                } => (provider, current) = (to.clone(), account_id.clone()),
                EventBody::SessionStatusChanged { status: to, .. } => status = *to,
                _ => {}
            }
        }
        let (repo, project) = match source.project_id {
            Some(project_id) => self.resolve_repo(None, Some(project_id))?,
            None => self.resolve_repo(Some(created_repo.clone()), None)?,
        };
        let default_account = project.and_then(|project| project.default_account);
        let account_id =
            self.fork_account(&provider, account_id, current.clone(), default_account)?;
        let repo_path = Path::new(&repo);
        // A folder without a commit holds no checkpoints: the fork works in it as it is.
        let checkpoint = if worktree::in_place(repo_path).await {
            None
        } else {
            match checkpoint::latest(repo_path, &original)
                .await
                .map_err(worktree_error)?
            {
                Some(local) => Some(local),
                None => checkpoint::fetch_latest(repo_path, &original, checkpoint::PUSH_TIMEOUT)
                    .await
                    .map_err(worktree_error)?,
            }
        };
        let session_id = SessionId::new(ulid::Ulid::new().to_string());
        let slug = worktree::slug(&session_id);
        let worktree = inner
            .worktrees
            .restore(repo_path, &slug, checkpoint.as_deref())
            .await
            .map_err(worktree_error)?;
        attachments::keep(&inner.attachments, &session_id, source.attachments).await?;
        // The fork tracks none of the original's pull requests, and owns only its own branch.
        let mut events: Vec<Event> = source
            .events
            .into_iter()
            .filter(|event| {
                !matches!(
                    event.body,
                    EventBody::PrLinked { .. }
                        | EventBody::PrUpdated { .. }
                        | EventBody::PrUnlinked { .. }
                        | EventBody::BranchCheckedOut { .. }
                )
            })
            .collect();
        for (seq, event) in (1..).zip(&mut events) {
            event.seq = seq;
            event.session_id = session_id.clone();
        }
        if let EventBody::SessionCreated {
            repo: created_repo,
            worktree: created_worktree,
            branch,
            ..
        } = &mut events[0].body
        {
            created_repo.clone_from(&repo);
            *created_worktree = worktree.path.to_string_lossy().into_owned();
            branch.clone_from(&worktree.branch);
        }
        let rerun = open_prompt(&events);
        let journal = &inner.journal;
        journal.import(events).await.map_err(internal)?;
        let body = EventBody::SessionForked {
            from_session: original.clone(),
            from_host: source.host_id.clone(),
        };
        journal
            .record(session_id.clone(), by.clone(), body)
            .await
            .map_err(internal)?;
        if account_id != current {
            let body = EventBody::AccountSwitched {
                account_id: account_id.clone(),
            };
            journal
                .record(session_id.clone(), by, body)
                .await
                .map_err(internal)?;
        }
        if matches!(status, SessionStatus::Archived | SessionStatus::Moved) {
            let body = EventBody::SessionStatusChanged {
                status: SessionStatus::Idle,
                retry_at: None,
            };
            journal
                .record(session_id.clone(), None, body)
                .await
                .map_err(internal)?;
        }
        let session = journal
            .session(session_id.clone())
            .await
            .map_err(internal)?
            .ok_or_else(|| super::not_found(&session_id))?;
        let open = matches!(
            session.status,
            SessionStatus::Running | SessionStatus::NeedsYou | SessionStatus::WaitingForCapacity
        );
        let rerun = rerun.filter(|_| open);
        if open {
            let (why, then) = match &rerun {
                Some(prompt) => {
                    // Queued before the turn closes, so it is not settled as if nothing ran.
                    journal
                        .set_queued_prompts(session_id.clone(), vec![prompt.clone()])
                        .await
                        .map_err(internal)?;
                    (
                        "the session was handed off during this turn; its prompt runs again here",
                        SessionStatus::Running,
                    )
                }
                None => (
                    "the session was handed off during this turn",
                    SessionStatus::NeedsYou,
                ),
            };
            actor::close_abandoned_turn(journal, &inner.tasks, &session, why, then)
                .await
                .map_err(internal)?;
        }
        if let Some(prs) = inner.prs.get()
            && worktree.branch.is_some()
        {
            prs.install(&session_id, &worktree.path).await;
        }
        // The fork's actor runs the queued prompt; were it not to start, the next daemon would.
        if rerun.is_some()
            && let Err(err) = self.actor(&session_id).await
        {
            warn!(%session_id, "cannot run the handed-off prompt: {}", err.message);
        }
        info!(
            %session_id,
            forked_from = %original,
            from_host = %source.host_id,
            %account_id,
            worktree = %worktree.path.display(),
            checkpoint = checkpoint.as_deref().unwrap_or("none"),
            "forked a session"
        );
        Ok(Forked {
            session_id,
            account_id,
            forked_from: original,
            from_host_id: source.host_id,
            worktree: worktree.path.to_string_lossy().into_owned(),
            branch: worktree.branch,
            checkpoint,
        })
    }
    /// The account of `provider` a fork runs on: `asked` when given, else
    /// `current`, the one it ran on, else the project's default, else the first one here.
    fn fork_account(
        &self,
        provider: &Provider,
        asked: Option<AccountId>,
        current: AccountId,
        default: Option<AccountId>,
    ) -> Result<AccountId, ErrorInfo> {
        let inner = &self.inner;
        let runs = |id: &AccountId| {
            inner
                .account(id)
                .is_some_and(|account| account.provider == *provider)
        };
        if let Some(asked) = asked {
            let account = inner.account(&asked).ok_or_else(|| {
                error(
                    ErrorCode::NotFound,
                    format!("account {asked} does not exist"),
                )
            })?;
            if account.provider != *provider {
                return Err(error(
                    ErrorCode::BadRequest,
                    format!(
                        "account {asked} runs {}, not {}; fork onto a {} account, then switch \
                         the provider",
                        account.provider.as_str(),
                        provider.as_str(),
                        provider.as_str()
                    ),
                ));
            }
            return Ok(asked);
        }
        if runs(&current) {
            return Ok(current);
        }
        if let Some(default) = default.filter(runs) {
            return Ok(default);
        }
        inner
            .accounts_lock()
            .iter()
            .find(|(_, account)| account.provider == *provider)
            .map(|(id, _)| id.clone())
            .ok_or_else(|| {
                error(
                    ErrorCode::NotFound,
                    format!(
                        "this host has no {} account to run the session on",
                        provider.as_str()
                    ),
                )
            })
    }

    /// Makes `session_id` read-only here, as another host took it over and goes on with it:
    /// stops its CLI and fails its open turn ([`SessionStatus::Moved`]).
    pub async fn moved_away(&self, session_id: SessionId) -> Result<(), ErrorInfo> {
        self.send(session_id, None, actor::Request::MovedAway)
            .await
            .map(|_| ())
    }
}

/// The prompt a user sent for a turn `events` leave open, to run again; `None` when no turn
/// is open, or an agent's message or a follow-up opened it.
fn open_prompt(events: &[Event]) -> Option<QueuedPrompt> {
    let mut open = None;
    for event in events {
        match &event.body {
            EventBody::ItemAdded { item } => {
                if let ItemBody::UserMessage { text, attachments } = &item.body {
                    open = (item.agent_message.is_none() && item.follow_up.is_none()).then(|| {
                        let prompt = QueuedPrompt {
                            prompt_id: actor::new_prompt_id(),
                            agent_message: None,
                            by: event.by.clone(),
                            text: text.clone(),
                            attachments: attachments.clone(),
                            retry: false,
                            retry_at: None,
                        };
                        (item.turn_id.clone(), prompt)
                    });
                }
            }
            EventBody::TurnCompleted { turn_id, .. }
            | EventBody::TurnInterrupted { turn_id }
            | EventBody::TurnFailed { turn_id, .. }
                if open.as_ref().is_some_and(|(open, _)| open == turn_id) =>
            {
                open = None;
            }
            _ => {}
        }
    }
    open.map(|(_, prompt)| prompt)
}
