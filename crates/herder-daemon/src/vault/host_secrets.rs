//! The host side of secrets: what a host's sessions use, fetched from its vault on demand.
//!
//! Requests go over the replication connection ([`super::Replicator`]), which attaches itself
//! here while it is open. Fetched values are kept in memory only, per session and secret as
//! the session named it, for [`CACHE_TTL`]; never on disk. Each fetch from the vault is audited
//! there, so a value served from this cache was audited for that session when fetched.
//!
//! A host not linked to a vault has no secrets ([`SecretErrorCode::NoVault`]). A linked host
//! whose vault is not connected, or does not answer within [`ANSWER_TIMEOUT`], serves only what
//! it has cached ([`SecretErrorCode::VaultUnreachable`]).

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use herder_protocol::{
    FetchedSecret, SecretError, SecretErrorCode, SecretInfo, SecretRef, SecretValue, SessionId,
};
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;

/// How long a fetched value is kept in memory.
pub const CACHE_TTL: Duration = Duration::from_secs(5 * 60);

/// How long the vault gets to answer a request.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(10);

/// What a host's sessions get of its vault's secrets.
pub struct HostSecrets {
    ttl: Duration,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    /// Whether the host is linked to a vault.
    linked: bool,
    /// The open replication connection, by its number, and where its requests go.
    connection: Option<(u64, mpsc::UnboundedSender<Ask>)>,
    /// Connections attached so far, to number the next.
    attached: u64,
    cache: HashMap<(SessionId, SecretRef), Cached>,
}

struct Cached {
    set: String,
    value: SecretValue,
    until: Instant,
}

/// A request for the vault, sent on the replication connection.
pub(super) enum Ask {
    /// The secrets granted to this host, without values.
    List(oneshot::Sender<Vec<SecretInfo>>),
    /// Values, for a session.
    Fetch {
        session_id: SessionId,
        secrets: Vec<SecretRef>,
        answer: oneshot::Sender<Result<Vec<FetchedSecret>, SecretError>>,
    },
}

/// Detaches its connection when dropped.
pub(super) struct Attached<'a> {
    secrets: &'a HostSecrets,
    number: u64,
}

impl Drop for Attached<'_> {
    fn drop(&mut self) {
        let mut state = self.secrets.lock();
        if state
            .connection
            .as_ref()
            .is_some_and(|(number, _)| *number == self.number)
        {
            state.connection = None;
        }
    }
}

impl Default for HostSecrets {
    fn default() -> Self {
        Self::with_ttl(CACHE_TTL)
    }
}

impl HostSecrets {
    /// Secrets whose values are kept for `ttl`; [`CACHE_TTL`] but in tests.
    pub fn with_ttl(ttl: Duration) -> Self {
        Self {
            ttl,
            state: Mutex::default(),
        }
    }

    /// Records whether the host is linked to a vault; a change forgets every value held.
    pub fn set_linked(&self, linked: bool) {
        let mut state = self.lock();
        if state.linked != linked {
            state.linked = linked;
            state.cache.clear();
            state.connection = None;
        }
    }

    /// Sends requests to a connection that just opened, in place of any earlier one, until
    /// the returned guard drops.
    pub(super) fn attach(&self) -> (Attached<'_>, mpsc::UnboundedReceiver<Ask>) {
        let (asks, received) = mpsc::unbounded_channel();
        let mut state = self.lock();
        state.attached += 1;
        let number = state.attached;
        state.connection = Some((number, asks));
        let attached = Attached {
            secrets: self,
            number,
        };
        (attached, received)
    }

    /// The secrets granted to this host, without values, ordered by set, then name.
    pub async fn list(&self) -> Result<Vec<SecretInfo>, SecretError> {
        let connection = self.connection()?;
        let (answer, answered) = oneshot::channel();
        ask(&connection, Ask::List(answer), answered).await
    }

    /// The values of `secrets` for `session_id`, in the order asked: from memory while fresh,
    /// otherwise from the vault, which records the fetch.
    pub async fn fetch(
        &self,
        session_id: &SessionId,
        secrets: &[SecretRef],
    ) -> Result<Vec<FetchedSecret>, SecretError> {
        let connection = {
            let mut state = self.lock();
            if !state.linked {
                return Err(no_vault());
            }
            let now = Instant::now();
            state.cache.retain(|_, cached| cached.until > now);
            let cached: Option<Vec<FetchedSecret>> = secrets
                .iter()
                .map(|secret| {
                    let cached = state.cache.get(&(session_id.clone(), secret.clone()))?;
                    Some(FetchedSecret {
                        secret: secret.clone(),
                        set: cached.set.clone(),
                        value: cached.value.clone(),
                    })
                })
                .collect();
            if let Some(cached) = cached {
                return Ok(cached);
            }
            state.connection.as_ref().map(|(_, asks)| asks.clone())
        };
        let connection = connection.ok_or_else(unreachable)?;
        let (answer, answered) = oneshot::channel();
        let request = Ask::Fetch {
            session_id: session_id.clone(),
            secrets: secrets.to_vec(),
            answer,
        };
        let fetched = ask(&connection, request, answered).await??;
        let until = Instant::now() + self.ttl;
        let mut state = self.lock();
        // Unlinked meanwhile: nothing is kept for a vault no longer linked.
        if !state.linked {
            return Ok(fetched);
        }
        for secret in &fetched {
            let cached = Cached {
                set: secret.set.clone(),
                value: secret.value.clone(),
                until,
            };
            state
                .cache
                .insert((session_id.clone(), secret.secret.clone()), cached);
        }
        Ok(fetched)
    }

    /// Where requests for the vault go now.
    fn connection(&self) -> Result<mpsc::UnboundedSender<Ask>, SecretError> {
        let state = self.lock();
        if !state.linked {
            return Err(no_vault());
        }
        let connection = state.connection.as_ref().map(|(_, asks)| asks.clone());
        connection.ok_or_else(unreachable)
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        // Every update is a field write or a map change, so a poisoned state is consistent.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Sends `request` on `connection` and waits for its answer.
async fn ask<T>(
    connection: &mpsc::UnboundedSender<Ask>,
    request: Ask,
    answered: oneshot::Receiver<T>,
) -> Result<T, SecretError> {
    connection.send(request).map_err(|_| unreachable())?;
    match tokio::time::timeout(ANSWER_TIMEOUT, answered).await {
        Ok(Ok(answer)) => Ok(answer),
        // The connection dropped before the vault answered.
        Ok(Err(_)) | Err(_) => Err(unreachable()),
    }
}

fn no_vault() -> SecretError {
    refusal(
        SecretErrorCode::NoVault,
        "this host is not linked to a vault, so it has no secrets",
    )
}

fn unreachable() -> SecretError {
    refusal(
        SecretErrorCode::VaultUnreachable,
        "this host cannot reach its vault now; try again shortly",
    )
}

fn refusal(code: SecretErrorCode, message: &str) -> SecretError {
    SecretError {
        code,
        message: message.to_owned(),
        secret: None,
        candidates: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    fn named(name: &str) -> SecretRef {
        name.parse().unwrap()
    }

    /// A vault that answers every fetch with `value-<NAME>` from the set `prod`, counting
    /// fetches, until `asks` closes.
    fn answer_fetches(mut asks: mpsc::UnboundedReceiver<Ask>) -> Arc<AtomicUsize> {
        let fetches = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&fetches);
        tokio::spawn(async move {
            while let Some(ask) = asks.recv().await {
                match ask {
                    Ask::Fetch {
                        secrets, answer, ..
                    } => {
                        counted.fetch_add(1, Ordering::SeqCst);
                        let values = secrets
                            .into_iter()
                            .map(|secret| FetchedSecret {
                                value: SecretValue::new(format!("value-{}", secret.name)).unwrap(),
                                set: "prod".into(),
                                secret,
                            })
                            .collect();
                        let _ = answer.send(Ok(values));
                    }
                    Ask::List(answer) => {
                        let _ = answer.send(Vec::new());
                    }
                }
            }
        });
        fetches
    }

    #[tokio::test(start_paused = true)]
    async fn values_are_kept_for_the_ttl_per_session_then_fetched_again() {
        let secrets = HostSecrets::with_ttl(Duration::from_secs(60));
        let session = SessionId::new("s1");
        assert_eq!(
            secrets
                .fetch(&session, &[named("TOKEN")])
                .await
                .unwrap_err()
                .code,
            SecretErrorCode::NoVault
        );
        secrets.set_linked(true);
        assert_eq!(
            secrets
                .fetch(&session, &[named("TOKEN")])
                .await
                .unwrap_err()
                .code,
            SecretErrorCode::VaultUnreachable
        );

        let (attached, asks) = secrets.attach();
        let fetches = answer_fetches(asks);
        let fetched = secrets.fetch(&session, &[named("TOKEN")]).await.unwrap();
        assert_eq!(fetched[0].value.expose(), "value-TOKEN");
        assert_eq!(fetched[0].set, "prod");
        assert_eq!(fetches.load(Ordering::SeqCst), 1);

        // Within the TTL, from memory, even with the vault gone.
        drop(attached);
        tokio::time::advance(Duration::from_secs(59)).await;
        let cached = secrets.fetch(&session, &[named("TOKEN")]).await.unwrap();
        assert_eq!(cached, fetched);
        assert_eq!(fetches.load(Ordering::SeqCst), 1);
        // Another session, or a secret not fetched yet, needs the vault.
        let other = SessionId::new("s2");
        assert_eq!(
            secrets
                .fetch(&other, &[named("TOKEN")])
                .await
                .unwrap_err()
                .code,
            SecretErrorCode::VaultUnreachable
        );
        assert_eq!(
            secrets
                .fetch(&session, &[named("TOKEN"), named("OTHER")])
                .await
                .unwrap_err()
                .code,
            SecretErrorCode::VaultUnreachable
        );

        // Past the TTL the value is gone: unreachable, then fetched afresh.
        tokio::time::advance(Duration::from_secs(2)).await;
        assert_eq!(
            secrets
                .fetch(&session, &[named("TOKEN")])
                .await
                .unwrap_err()
                .code,
            SecretErrorCode::VaultUnreachable
        );
        let (_attached, asks) = secrets.attach();
        let fetches = answer_fetches(asks);
        secrets.fetch(&session, &[named("TOKEN")]).await.unwrap();
        assert_eq!(fetches.load(Ordering::SeqCst), 1);

        // Unlinking forgets everything.
        secrets.set_linked(false);
        assert_eq!(
            secrets
                .fetch(&session, &[named("TOKEN")])
                .await
                .unwrap_err()
                .code,
            SecretErrorCode::NoVault
        );
        secrets.set_linked(true);
        assert_eq!(
            secrets
                .fetch(&session, &[named("TOKEN")])
                .await
                .unwrap_err()
                .code,
            SecretErrorCode::VaultUnreachable
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_vault_that_never_answers_is_unreachable() {
        let secrets = HostSecrets::default();
        secrets.set_linked(true);
        let (_attached, _asks) = secrets.attach();
        let error = secrets.list().await.unwrap_err();
        assert_eq!(error.code, SecretErrorCode::VaultUnreachable);
    }
}
