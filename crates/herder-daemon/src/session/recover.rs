//! Recovery: a session whose host died goes on here, from the journal its vault holds.
//!
//! The session keeps its id, so it stays one session wherever clients follow it, and its
//! events keep their seqs, `at` and `by`. Only its `session_created` changes: it names this
//! host's clone of the repository and the new worktree. The worktree is restored from the
//! session's latest checkpoint on `origin` ([`checkpoint::fetch_latest`]), on the session's
//! own branch. A turn the dead host left open is closed as after a restart, and the session
//! moves to an account of this host by `account_switched` when its own is not here; its next
//! prompt starts the CLI seeded with the transcript, as after any switch. The images its
//! prompts carried are kept here under their ids, so `get_attachment` answers as before.
//!
//! The host it came from, if it comes back, makes its own copy read-only
//! ([`SessionManager::moved_away`]).

use std::path::Path;

use herder_protocol::{
    AccountId, AttachmentId, ErrorCode, ErrorInfo, Event, EventBody, Image, ProjectId, Provider,
    SessionId, SessionStatus,
};
use tracing::info;

use super::{SessionManager, actor, attachments, error, internal, worktree_error};
use crate::worktree::{self, checkpoint};

/// A session recovered here.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Recovered {
    /// The session, under the id it had.
    pub session_id: SessionId,
    /// The account it runs on here.
    pub account_id: AccountId,
    /// Its new worktree.
    pub worktree: String,
    /// The branch checked out there.
    pub branch: String,
    /// The checkpoint ref the worktree was restored from; `None` when `origin` had none, so
    /// the worktree starts at the default branch.
    pub checkpoint: Option<String>,
}

impl SessionManager {
    /// Takes over a session from another host: `events` is its whole journal, as the vault
    /// holds it, `images` the images its prompts carried, by attachment id, and `project_id`
    /// its project, whose clone here it works in. It runs on
    /// `account_id`, or else on its own account, its project's default account or this host's
    /// first account of its provider.
    pub async fn recover(
        &self,
        events: Vec<Event>,
        images: Vec<(AttachmentId, Image)>,
        project_id: ProjectId,
        account_id: Option<AccountId>,
    ) -> Result<Recovered, ErrorInfo> {
        let inner = &self.inner;
        let Some(first) = events.first() else {
            return Err(error(ErrorCode::BadRequest, "the journal is empty"));
        };
        let session_id = first.session_id.clone();
        let EventBody::SessionCreated {
            branch,
            parent,
            provider: created_on,
            account_id: created_account,
            ..
        } = &first.body
        else {
            return Err(error(
                ErrorCode::BadRequest,
                format!("the journal of {session_id} does not start with its creation"),
            ));
        };
        if !events
            .iter()
            .map(|event| event.seq)
            .eq(1..=events.len() as u64)
            || events.iter().any(|event| event.session_id != session_id)
        {
            return Err(error(
                ErrorCode::BadRequest,
                format!(
                    "the journal of {session_id} has gaps; this herder may be older than the \
                     host that recorded it"
                ),
            ));
        }
        if parent.is_some() {
            return Err(error(
                ErrorCode::Unsupported,
                format!("{session_id} is a child of a task; tasks cannot be recovered yet"),
            ));
        }
        let journal = &inner.journal;
        if journal
            .session(session_id.clone())
            .await
            .map_err(internal)?
            .is_some()
        {
            return Err(error(
                ErrorCode::Conflict,
                format!("session {session_id} is on this host already; a session never moves back"),
            ));
        }
        // Where the session stands at the end of its journal.
        let mut provider = created_on.clone();
        let mut current = created_account.clone();
        let mut status = SessionStatus::Idle;
        for event in &events {
            match &event.body {
                EventBody::AccountSwitched { account_id } => current = account_id.clone(),
                EventBody::ProviderSwitched {
                    provider: to,
                    account_id,
                    ..
                } => (provider, current) = (to.clone(), account_id.clone()),
                EventBody::SessionStatusChanged { status: to } => status = *to,
                _ => {}
            }
        }
        if matches!(status, SessionStatus::Archived | SessionStatus::Moved) {
            return Err(error(
                ErrorCode::Conflict,
                format!("session {session_id} is read-only; there is nothing to recover"),
            ));
        }
        let (repo, project) = self.resolve_repo(None, Some(project_id))?;
        let default_account = project.and_then(|project| project.default_account);
        let account_id =
            self.recovery_account(&provider, account_id, current.clone(), default_account)?;
        let checkpoint =
            checkpoint::fetch_latest(Path::new(&repo), &session_id, checkpoint::PUSH_TIMEOUT)
                .await
                .map_err(worktree_error)?;
        let worktree = inner
            .worktrees
            .restore(
                Path::new(&repo),
                &worktree::slug(&session_id),
                branch.clone(),
                checkpoint.as_deref(),
            )
            .await
            .map_err(worktree_error)?;
        attachments::keep(&inner.attachments, &session_id, images).await?;
        let mut events = events;
        if let EventBody::SessionCreated {
            repo: created_repo,
            worktree: created_worktree,
            ..
        } = &mut events[0].body
        {
            created_repo.clone_from(&repo);
            *created_worktree = worktree.path.to_string_lossy().into_owned();
        }
        journal.import(events).await.map_err(internal)?;
        if account_id != current {
            let body = EventBody::AccountSwitched {
                account_id: account_id.clone(),
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
        if matches!(
            session.status,
            SessionStatus::Running | SessionStatus::NeedsYou | SessionStatus::WaitingForCapacity
        ) {
            actor::close_abandoned_turn(journal, &inner.tasks, &session)
                .await
                .map_err(internal)?;
        }
        if let Some(prs) = inner.prs.get() {
            prs.install(&session_id, &worktree.path).await;
        }
        info!(
            %session_id,
            %account_id,
            worktree = %worktree.path.display(),
            checkpoint = checkpoint.as_deref().unwrap_or("none"),
            "recovered a session from another host"
        );
        Ok(Recovered {
            session_id,
            account_id,
            worktree: worktree.path.to_string_lossy().into_owned(),
            branch: worktree.branch,
            checkpoint,
        })
    }

    /// The account of `provider` a recovered session runs on: `asked` when given, else
    /// `current`, the one it ran on, else the project's default, else the first one here.
    fn recovery_account(
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
                        "account {asked} runs {}, not {}; recover on a {} account, then switch \
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

    /// Makes `session_id` read-only here, as another host recovered it and goes on with it:
    /// stops its CLI and fails its open turn ([`SessionStatus::Moved`]).
    pub async fn moved_away(&self, session_id: SessionId) -> Result<(), ErrorInfo> {
        self.send(session_id, None, actor::Request::MovedAway)
            .await
            .map(|_| ())
    }
}
