//! The store behind an async face: every call runs on the blocking pool, and every stored
//! event is published to the sink.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError, RwLock};

use anyhow::{Context, Result, anyhow};
use herder_protocol::{
    CommandId, CommandResult, Event, EventBody, FailoverTotal, HostId, JournalRecord, Project,
    ProjectId, PromptId, PullRequest, Seq, SessionHead, SessionId, SessionStatus, SessionSummary,
    Timestamp, UsageTotal, UserId,
};
use herder_store::{NativeSession, NewEvent, QueuedPrompt, Session, Store};

use super::EventSink;

/// The journal shared by every session actor.
#[derive(Clone)]
pub(crate) struct Journal {
    store: Arc<Mutex<Store>>,
    sink: Arc<dyn EventSink>,
    /// The projects discovery knows.
    projects: Arc<RwLock<Projects>>,
}

/// The project list discovery last published, and the project of each clone in it.
#[derive(Default)]
pub(crate) struct Projects {
    pub(crate) list: Vec<Project>,
    by_path: HashMap<String, ProjectId>,
}

impl Projects {
    /// The project whose clone `repo` is, if discovery knows it.
    pub(crate) fn of_repo(&self, repo: &str) -> Option<&Project> {
        let id = self.by_path.get(repo)?;
        self.list.iter().find(|project| project.project_id == *id)
    }
}

/// What a session was created with that lists do not show.
#[derive(Debug, Default)]
pub(crate) struct Settings {
    /// Its own failover pin.
    pub(crate) failover_pin: Option<bool>,
}

impl Journal {
    pub(super) fn new(store: Store, sink: Arc<dyn EventSink>) -> Self {
        Self {
            store: Arc::new(Mutex::new(store)),
            sink,
            projects: Arc::default(),
        }
    }

    pub(super) fn sink(&self) -> &dyn EventSink {
        &*self.sink
    }

    /// Appends an event now and publishes it once stored, followed by the session list when
    /// the event changes what it shows. Publishing happens under the store lock, so events and
    /// lists reach the sink in order whichever task records them.
    pub(crate) async fn record(
        &self,
        session_id: SessionId,
        by: Option<UserId>,
        body: EventBody,
    ) -> Result<Event> {
        self.append(session_id, by, body, None).await
    }

    /// Records `body`, the `user_message` item of the queued prompt `prompt_id`, and takes that
    /// prompt off the stored queue with it ([`Store::append_prompt`]).
    pub(super) async fn record_prompt(
        &self,
        session_id: SessionId,
        by: Option<UserId>,
        body: EventBody,
        prompt_id: PromptId,
    ) -> Result<Event> {
        self.append(session_id, by, body, Some(prompt_id)).await
    }

    async fn append(
        &self,
        session_id: SessionId,
        by: Option<UserId>,
        body: EventBody,
        prompt_id: Option<PromptId>,
    ) -> Result<Event> {
        let lists = matches!(
            body,
            EventBody::SessionCreated { .. }
                | EventBody::SessionStatusChanged { .. }
                | EventBody::AccountSwitched { .. }
                | EventBody::ProviderSwitched { .. }
                | EventBody::TitleChanged { .. }
        );
        let event = NewEvent {
            session_id,
            at: Timestamp::now(),
            by,
            body,
        };
        let sink = self.sink.clone();
        let projects = self.projects.clone();
        self.with_store(move |store| {
            let stored = match &prompt_id {
                Some(prompt_id) => store.append_prompt(event, prompt_id)?,
                None => store.append(event)?,
            };
            // Usage is recorded here and not on import: a forked history's turns ran, and
            // were counted, on the host it came from.
            if let EventBody::TurnCompleted {
                turn_id,
                usage: Some(usage),
            } = &stored.body
                && let Err(err) =
                    store.record_turn_usage(&stored.session_id, turn_id, usage, stored.at)
            {
                tracing::warn!(%turn_id, "cannot record a turn's usage: {err:#}");
            }
            sink.event(&stored);
            if lists {
                let projects = projects.read().unwrap_or_else(PoisonError::into_inner);
                sink.sessions_changed(&heads(store, &projects)?);
            }
            Ok(stored)
        })
        .await
        .context("appending to the journal")
    }

    /// Appends a session's whole journal as another host recorded it, keeping each event's
    /// `at` and `by`, and publishes it with the session list. The session must be new here, so
    /// its events keep their seqs.
    pub(super) async fn import(&self, events: Vec<Event>) -> Result<()> {
        let sink = self.sink.clone();
        let projects = self.projects.clone();
        self.with_store(move |store| {
            for event in events {
                let stored = store.append(NewEvent {
                    session_id: event.session_id,
                    at: event.at,
                    by: event.by,
                    body: event.body,
                })?;
                sink.event(&stored);
            }
            let projects = projects.read().unwrap_or_else(PoisonError::into_inner);
            sink.sessions_changed(&heads(store, &projects)?);
            Ok(())
        })
        .await
        .context("appending to the journal")
    }

    pub(crate) async fn read_since(
        &self,
        session_id: SessionId,
        after_seq: Seq,
        limit: usize,
    ) -> Result<Vec<Event>> {
        self.with_store(move |store| store.read_since(&session_id, after_seq, limit))
            .await
    }

    /// Up to `limit` events of a session after `after_seq`, as stored; for replication.
    pub(crate) async fn records_since(
        &self,
        session_id: SessionId,
        after_seq: Seq,
        limit: usize,
    ) -> Result<Vec<JournalRecord>> {
        self.with_store(move |store| store.read_records_since(&session_id, after_seq, limit))
            .await
    }

    /// Every session as the vault's fleet index lists it, ordered by session id; a session
    /// whose repo discovery has not resolved yet gets the local project id of `host`.
    pub(crate) async fn summaries(&self, host: &HostId) -> Result<Vec<SessionSummary>> {
        let sessions = self
            .with_store(|store| {
                let sessions = store.sessions()?;
                sessions
                    .into_iter()
                    .map(|session| Ok((store.session_prs(&session.session_id)?, session)))
                    .collect::<herder_store::Result<Vec<_>>>()
            })
            .await?;
        let projects = self.projects();
        Ok(sessions
            .into_iter()
            .map(|(prs, session)| SessionSummary {
                project_id: projects
                    .by_path
                    .get(&session.repo)
                    .cloned()
                    .unwrap_or_else(|| ProjectId::local(host, &session.repo)),
                session_id: session.session_id,
                repo: session.repo,
                branch: session.branch,
                status: session.status,
                prs,
                parent: session.parent,
                parent_host: session.parent_host,
                task: session.task,
                title: session.title,
                head_seq: session.last_seq,
                updated_at: session.updated_at,
            })
            .collect())
    }

    /// Every event of a session, oldest first.
    pub(crate) async fn all(&self, session_id: SessionId) -> Result<Vec<Event>> {
        self.read_since(session_id, 0, usize::MAX).await
    }

    pub(crate) async fn session(&self, session_id: SessionId) -> Result<Option<Session>> {
        self.with_store(move |store| store.session(&session_id))
            .await
    }

    /// Every branch the session owns, in the order first seen.
    pub(crate) async fn branches(&self, session_id: SessionId) -> Result<Vec<String>> {
        self.with_store(move |store| store.session_branches(&session_id))
            .await
    }

    /// Pull requests tracked for the session, ordered by number.
    pub(crate) async fn prs(&self, session_id: SessionId) -> Result<Vec<PullRequest>> {
        self.with_store(move |store| store.session_prs(&session_id))
            .await
    }

    /// Every child session of `parent`'s task, ordered by session id.
    pub(crate) async fn children(&self, parent: SessionId) -> Result<Vec<Session>> {
        self.with_store(move |store| store.children(&parent)).await
    }

    /// The usage of the turns completed since `since`, per account and model.
    pub(super) async fn usage_totals(&self, since: Timestamp) -> Result<Vec<UsageTotal>> {
        self.with_store(move |store| store.usage_totals(since))
            .await
    }

    /// Each account's limit hits and failovers since `since`.
    pub(super) async fn failover_totals(&self, since: Timestamp) -> Result<Vec<FailoverTotal>> {
        self.with_store(move |store| store.failover_totals(since))
            .await
    }

    pub(crate) async fn sessions(&self) -> Result<Vec<Session>> {
        self.with_store(|store| store.sessions()).await
    }

    /// The result `user`'s command `command_id` was accepted with, if it is remembered.
    pub(super) async fn command_result(
        &self,
        user: UserId,
        command_id: CommandId,
    ) -> Result<Option<CommandResult>> {
        self.with_store(move |store| store.command_result(&user, &command_id))
            .await
    }

    /// Remembers that `user`'s command `command_id` was accepted with `result`.
    pub(super) async fn record_command_result(
        &self,
        user: UserId,
        command_id: CommandId,
        result: CommandResult,
    ) -> Result<()> {
        self.with_store(move |store| store.record_command_result(&user, &command_id, &result))
            .await
    }

    /// The prompts queued in a session, oldest first.
    pub(super) async fn queued_prompts(&self, session_id: SessionId) -> Result<Vec<QueuedPrompt>> {
        self.with_store(move |store| store.queued_prompts(&session_id))
            .await
    }

    /// The original payload accepted under a sender-scoped delivery key.
    pub(super) async fn agent_message_text(
        &self,
        recipient: SessionId,
        sender: SessionId,
        message_id: String,
    ) -> Result<Option<String>> {
        self.with_store(move |store| store.agent_message_text(&recipient, &sender, &message_id))
            .await
    }

    /// Replaces the prompts queued in a session, and publishes the session list with them.
    pub(super) async fn set_queued_prompts(
        &self,
        session_id: SessionId,
        prompts: Vec<QueuedPrompt>,
    ) -> Result<()> {
        let sink = self.sink.clone();
        let projects = self.projects.clone();
        self.with_store(move |store| {
            store.set_queued_prompts(&session_id, &prompts)?;
            let projects = projects.read().unwrap_or_else(PoisonError::into_inner);
            sink.sessions_changed(&heads(store, &projects)?);
            Ok(())
        })
        .await
    }

    /// The CLI session last reported behind a session.
    pub(super) async fn native_session(
        &self,
        session_id: SessionId,
    ) -> Result<Option<NativeSession>> {
        self.with_store(move |store| store.native_session(&session_id))
            .await
    }

    /// Records the CLI session behind a session.
    pub(super) async fn set_native_session(
        &self,
        session_id: SessionId,
        native: NativeSession,
    ) -> Result<()> {
        self.with_store(move |store| store.set_native_session(&session_id, &native))
            .await
    }

    /// Every session with a prompt queued.
    pub(super) async fn sessions_with_queued_prompts(&self) -> Result<Vec<SessionId>> {
        self.with_store(|store| store.sessions_with_queued_prompts())
            .await
    }

    pub(super) async fn heads(&self) -> Result<Vec<SessionHead>> {
        let projects = self.projects.clone();
        self.with_store(move |store| {
            let projects = projects.read().unwrap_or_else(PoisonError::into_inner);
            heads(store, &projects)
        })
        .await
    }

    /// The projects discovery last published.
    pub(crate) fn projects(&self) -> std::sync::RwLockReadGuard<'_, Projects> {
        // Every update is one assignment, so a poisoned lock holds a consistent list.
        self.projects.read().unwrap_or_else(PoisonError::into_inner)
    }

    /// What `session_id` was created with; the defaults for a session that does not exist.
    pub(crate) async fn settings(&self, session_id: SessionId) -> Result<Settings> {
        let first = self.read_since(session_id, 0, 1).await?;
        Ok(match first.into_iter().next().map(|event| event.body) {
            Some(EventBody::SessionCreated { failover_pin, .. }) => Settings { failover_pin },
            _ => Settings::default(),
        })
    }

    /// Resolves sessions' projects from `projects` from now on; returns whether any clone's
    /// project changed.
    pub(super) fn set_projects(&self, projects: &[Project]) -> bool {
        let by_path: HashMap<String, ProjectId> = projects
            .iter()
            .flat_map(|p| {
                p.paths
                    .iter()
                    .map(|path| (path.clone(), p.project_id.clone()))
            })
            .collect();
        let mut current = self
            .projects
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        let changed = current.by_path != by_path;
        *current = Projects {
            list: projects.to_vec(),
            by_path,
        };
        changed
    }

    async fn with_store<T: Send + 'static>(
        &self,
        call: impl FnOnce(&mut Store) -> herder_store::Result<T> + Send + 'static,
    ) -> Result<T> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || {
            let mut store = store
                .lock()
                .map_err(|_| anyhow!("the store lock is poisoned"))?;
            Ok(call(&mut store)?)
        })
        .await
        .context("the store task panicked")?
    }
}

/// Every session in `store` as lists show it, with its queue and its project as `projects`
/// resolve it and, for a primary, how many of its children need the user.
fn heads(store: &Store, projects: &Projects) -> herder_store::Result<Vec<SessionHead>> {
    let sessions = store.sessions()?;
    let mut queues = store.queues()?;
    let mut need_you: HashMap<SessionId, u32> = HashMap::new();
    for session in &sessions {
        if let Some(parent) = &session.parent
            && session.status == SessionStatus::NeedsYou
        {
            *need_you.entry(parent.clone()).or_default() += 1;
        }
    }
    Ok(sessions
        .into_iter()
        .map(|session| SessionHead {
            host_id: None,
            project_id: projects.by_path.get(&session.repo).cloned(),
            children_need_you: need_you.get(&session.session_id).copied().unwrap_or(0),
            queue: queues
                .remove(&session.session_id)
                .unwrap_or_default()
                .into_iter()
                .filter_map(listed)
                .collect(),
            session_id: session.session_id,
            head_seq: session.last_seq,
            status: session.status,
            parent: session.parent,
            parent_host: session.parent_host,
            task: session.task,
            title: session.title,
            account_id: session.account_id,
        })
        .collect())
}

/// A queued prompt as the session's queue lists it; `None` for a turn's prompt queued again
/// to retry it, which has started already.
fn listed(prompt: QueuedPrompt) -> Option<herder_protocol::QueuedPrompt> {
    let files = prompt
        .attachments
        .iter()
        .filter(|a| a.name.is_some())
        .count();
    let images = prompt.attachments.len() - files;
    (!prompt.retry).then(|| herder_protocol::QueuedPrompt {
        prompt_id: prompt.prompt_id,
        text: prompt.text,
        images: u32::try_from(images).unwrap_or(u32::MAX),
        files: u32::try_from(files).unwrap_or(u32::MAX),
        by: prompt.by,
        agent_message: prompt.agent_message,
    })
}
