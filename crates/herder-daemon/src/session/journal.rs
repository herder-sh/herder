//! The store behind an async face: every call runs on the blocking pool, and every stored
//! event is published to the sink.

use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow};
use herder_protocol::{
    Event, EventBody, PullRequest, Seq, SessionHead, SessionId, Timestamp, UserId,
};
use herder_store::{NewEvent, Session, Store};

use super::EventSink;

/// The journal shared by every session actor.
#[derive(Clone)]
pub(crate) struct Journal {
    store: Arc<Mutex<Store>>,
    sink: Arc<dyn EventSink>,
}

impl Journal {
    pub(super) fn new(store: Store, sink: Arc<dyn EventSink>) -> Self {
        Self {
            store: Arc::new(Mutex::new(store)),
            sink,
        }
    }

    pub(super) fn sink(&self) -> &dyn EventSink {
        &*self.sink
    }

    /// Appends an event now and publishes it once stored. Publishing happens under the store
    /// lock, so events reach the sink in seq order whichever task records them.
    pub(crate) async fn record(
        &self,
        session_id: SessionId,
        by: Option<UserId>,
        body: EventBody,
    ) -> Result<Event> {
        let event = NewEvent {
            session_id,
            at: Timestamp::now(),
            by,
            body,
        };
        let sink = self.sink.clone();
        self.with_store(move |store| {
            let stored = store.append(event)?;
            sink.event(&stored);
            Ok(stored)
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

    pub(crate) async fn sessions(&self) -> Result<Vec<Session>> {
        self.with_store(|store| store.sessions()).await
    }

    pub(super) async fn heads(&self) -> Result<Vec<SessionHead>> {
        let sessions = self.sessions().await?;
        Ok(sessions
            .into_iter()
            .map(|session| SessionHead {
                session_id: session.session_id,
                head_seq: session.last_seq,
            })
            .collect())
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
