//! The host side of secrets: what a host's sessions use, fetched from its vault on demand, and
//! what the host holds for its own use, such as a bot's token.
//!
//! Requests go over the replication connection ([`super::Replicator`]), which attaches itself
//! here while it is open. Fetched values are kept in memory only, never on disk. A session's
//! are kept per session and secret as the session named it, for [`CACHE_TTL`]. Each fetch from
//! the vault is audited there, so a value served from this cache was audited for that session
//! when fetched.
//!
//! What the host uses itself it holds ([`HostSecrets::hold`]): fetched for a purpose from the
//! sets granted to it for host use, audited under that purpose, fetched again every
//! [`CACHE_TTL`] to pick up a changed value or a revoked grant, and after a failure with
//! backoff. Its [`HostSecretsState`] says why a feature that needs the values has none.
//!
//! A host not linked to a vault has no secrets ([`SecretErrorCode::NoVault`]). A linked host
//! whose vault is not connected, or does not answer within [`ANSWER_TIMEOUT`], serves only what
//! it has in memory ([`SecretErrorCode::VaultUnreachable`]).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use herder_protocol::{
    FetchedSecret, HostSecretsState, SecretError, SecretErrorCode, SecretInfo, SecretRef,
    SecretValue, SessionId,
};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use super::store::SecretUse;

/// How long a fetched value is kept in memory.
pub const CACHE_TTL: Duration = Duration::from_secs(5 * 60);

/// How long the vault gets to answer a request.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(10);

/// The first wait before asking again for held secrets that could not be fetched; it doubles
/// with each failure, up to [`MAX_BACKOFF`].
const MIN_BACKOFF: Duration = Duration::from_secs(1);

/// The longest wait before asking again for held secrets that could not be fetched.
const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// What a host's sessions, and the host itself, get of its vault's secrets.
pub struct HostSecrets {
    ttl: Duration,
    state: Mutex<State>,
    /// Bumped whenever the link or the connection changes, so held secrets are asked for again.
    reconnected: watch::Sender<()>,
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
    /// Values, for a session or the host's own use.
    Fetch {
        used: SecretUse,
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
            drop(state);
            self.secrets.reconnected.send_replace(());
        }
    }
}

/// Secrets the host holds for its own use, fetched again while this lives; see
/// [`HostSecrets::hold`].
pub struct HeldSecrets {
    held: watch::Receiver<Held>,
    task: JoinHandle<()>,
}

impl Drop for HeldSecrets {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl HeldSecrets {
    /// What is held now.
    pub fn current(&self) -> Held {
        self.held.borrow().clone()
    }

    /// Follows what is held: its value now, and each change.
    pub fn watch(&self) -> watch::Receiver<Held> {
        self.held.clone()
    }
}

/// What the host holds of secrets for its own use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Held {
    /// Every value, in the order asked.
    Ok(Vec<FetchedSecret>),
    /// None, as the vault cannot be reached; the host keeps asking.
    WaitingForVault,
    /// None, as the vault, or the lack of one, refuses them; the host keeps asking, slowly.
    Denied(SecretError),
}

impl Held {
    /// Where the host stands, without the values, for a client.
    pub fn state(&self) -> HostSecretsState {
        match self {
            Self::Ok(_) => HostSecretsState::Ok,
            Self::WaitingForVault => HostSecretsState::WaitingForVault,
            Self::Denied(error) => HostSecretsState::Denied {
                error: error.clone(),
            },
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
            reconnected: watch::Sender::new(()),
        }
    }

    /// Records whether the host is linked to a vault; a change forgets every value cached, and
    /// held secrets are asked for again.
    pub fn set_linked(&self, linked: bool) {
        let mut state = self.lock();
        if state.linked != linked {
            state.linked = linked;
            state.cache.clear();
            state.connection = None;
            drop(state);
            self.reconnected.send_replace(());
        }
    }

    /// Whether the host is linked to a vault with secrets on.
    pub fn linked(&self) -> bool {
        self.lock().linked
    }

    /// Sends requests to a connection that just opened, in place of any earlier one, until
    /// the returned guard drops.
    pub(super) fn attach(&self) -> (Attached<'_>, mpsc::UnboundedReceiver<Ask>) {
        let (asks, received) = mpsc::unbounded_channel();
        let mut state = self.lock();
        state.attached += 1;
        let number = state.attached;
        state.connection = Some((number, asks));
        drop(state);
        self.reconnected.send_replace(());
        let attached = Attached {
            secrets: self,
            number,
        };
        (attached, received)
    }

    /// Holds the values of `secrets` for the host's own use under `purpose`, such as
    /// `bot:trip1-team`, while the returned handle lives: fetched now, again every
    /// [`CACHE_TTL`], and after a failure with backoff, sooner when the vault connects. A value
    /// the vault cannot be reached to refresh is kept for one more [`CACHE_TTL`]; a refusal,
    /// such as a revoked grant, drops the values at once.
    pub fn hold(self: &Arc<Self>, purpose: String, secrets: Vec<SecretRef>) -> HeldSecrets {
        let (publish, held) = watch::channel(Held::WaitingForVault);
        let task = tokio::spawn(Arc::clone(self).keep(purpose, secrets, publish));
        HeldSecrets { held, task }
    }

    /// Fetches `secrets` for `purpose`, publishing what is held, until [`HeldSecrets`] drops
    /// and aborts it.
    async fn keep(
        self: Arc<Self>,
        purpose: String,
        secrets: Vec<SecretRef>,
        publish: watch::Sender<Held>,
    ) {
        let mut reconnected = self.reconnected.subscribe();
        let mut backoff = MIN_BACKOFF;
        // When the values held were fetched.
        let mut fetched_at: Option<Instant> = None;
        loop {
            reconnected.mark_unchanged();
            let used = SecretUse::Host(purpose.clone());
            let wait = match self.ask_fetch(used, &secrets).await {
                Ok(values) => {
                    fetched_at = Some(Instant::now());
                    backoff = MIN_BACKOFF;
                    publish.send_if_modified(|held| replace(held, Held::Ok(values)));
                    self.ttl
                }
                Err(error) => {
                    let keep = error.code == SecretErrorCode::VaultUnreachable
                        && fetched_at.is_some_and(|at| at.elapsed() < self.ttl * 2);
                    if !keep {
                        fetched_at = None;
                        let held = if error.code == SecretErrorCode::VaultUnreachable {
                            Held::WaitingForVault
                        } else {
                            Held::Denied(error)
                        };
                        publish.send_if_modified(|current| replace(current, held));
                    }
                    let wait = backoff;
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                    wait
                }
            };
            // The sender of `reconnected` lives as long as `self`, which this task holds.
            tokio::select! {
                () = tokio::time::sleep(wait) => {}
                _ = reconnected.changed() => {}
            }
        }
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
        {
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
        }
        let used = SecretUse::Session(session_id.clone());
        let fetched = self.ask_fetch(used, secrets).await?;
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

    /// The values of `secrets` for `used`, from the vault.
    async fn ask_fetch(
        &self,
        used: SecretUse,
        secrets: &[SecretRef],
    ) -> Result<Vec<FetchedSecret>, SecretError> {
        let connection = self.connection()?;
        let (answer, answered) = oneshot::channel();
        let request = Ask::Fetch {
            used,
            secrets: secrets.to_vec(),
            answer,
        };
        ask(&connection, request, answered).await?
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

/// Puts `new` in `held`; whether that changed it.
fn replace(held: &mut Held, new: Held) -> bool {
    let changed = *held != new;
    *held = new;
    changed
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
        "this host is not linked to a vault with Secrets on, so it has no secrets; an owner \
         links it to one in the herder app, on the machine's page under Secrets",
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
                        used: SecretUse::Session(_),
                        secrets,
                        answer,
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
                    Ask::Fetch { .. } => panic!("only fetches for sessions are expected"),
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

    /// What a fake vault answers fetches for host use with: values of this text, or this
    /// refusal.
    type Answer = Arc<Mutex<Result<String, SecretError>>>;

    /// A vault that answers every fetch for host use as `answer` says, from the set `bots`,
    /// counting fetches, until `asks` closes.
    fn answer_host_fetches(
        mut asks: mpsc::UnboundedReceiver<Ask>,
        answer: Answer,
    ) -> Arc<AtomicUsize> {
        let fetches = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&fetches);
        tokio::spawn(async move {
            while let Some(ask) = asks.recv().await {
                let Ask::Fetch {
                    used: SecretUse::Host(purpose),
                    secrets,
                    answer: reply,
                } = ask
                else {
                    panic!("only fetches for host use are expected");
                };
                assert_eq!(purpose, "bot:team");
                counted.fetch_add(1, Ordering::SeqCst);
                let values = answer.lock().unwrap().clone().map(|text| {
                    secrets
                        .into_iter()
                        .map(|secret| FetchedSecret {
                            value: SecretValue::new(text.clone()).unwrap(),
                            set: "bots".into(),
                            secret,
                        })
                        .collect()
                });
                let _ = reply.send(values);
            }
        });
        fetches
    }

    fn value_held(held: &HeldSecrets) -> Option<String> {
        match held.current() {
            Held::Ok(values) => Some(values[0].value.expose().to_owned()),
            _ => None,
        }
    }

    /// Lets the task holding secrets run until it waits again.
    async fn settle() {
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test(start_paused = true)]
    async fn held_secrets_follow_the_vault_and_say_why_they_are_missing() {
        let secrets = Arc::new(HostSecrets::with_ttl(Duration::from_secs(60)));
        let held = secrets.hold("bot:team".into(), vec![named("SLACK_TOKEN")]);
        settle().await;
        let Held::Denied(error) = held.current() else {
            panic!("an unlinked host is refused: {:?}", held.current());
        };
        assert_eq!(error.code, SecretErrorCode::NoVault);
        assert_eq!(held.current().state(), HostSecretsState::Denied { error });

        // Linked, but the vault is not connected: waiting for it.
        secrets.set_linked(true);
        settle().await;
        assert_eq!(held.current(), Held::WaitingForVault);
        assert_eq!(held.current().state(), HostSecretsState::WaitingForVault);

        // The vault connects: fetched at once, without waiting out the backoff.
        let answer: Answer = Arc::new(Mutex::new(Ok("v1".into())));
        let (attached, asks) = secrets.attach();
        let fetches = answer_host_fetches(asks, Arc::clone(&answer));
        settle().await;
        assert_eq!(value_held(&held).as_deref(), Some("v1"));
        assert_eq!(held.current().state(), HostSecretsState::Ok);
        assert_eq!(fetches.load(Ordering::SeqCst), 1);

        // A changed value is picked up by the next fetch, a TTL on.
        *answer.lock().unwrap() = Ok("v2".into());
        let mut watch = held.watch();
        watch.mark_unchanged();
        tokio::time::advance(Duration::from_secs(59)).await;
        settle().await;
        assert_eq!(value_held(&held).as_deref(), Some("v1"));
        tokio::time::advance(Duration::from_secs(1)).await;
        settle().await;
        assert!(watch.has_changed().unwrap());
        assert_eq!(value_held(&held).as_deref(), Some("v2"));
        assert_eq!(fetches.load(Ordering::SeqCst), 2);

        // The vault gone, the values are kept one more TTL while asking again, then dropped.
        drop(attached);
        tokio::time::advance(Duration::from_secs(60)).await;
        settle().await;
        assert_eq!(value_held(&held).as_deref(), Some("v2"));
        tokio::time::advance(Duration::from_secs(60)).await;
        settle().await;
        assert_eq!(held.current(), Held::WaitingForVault);

        // Back, then a refusal, such as a revoked grant, drops the values at once.
        let (_attached, asks) = secrets.attach();
        let fetches = answer_host_fetches(asks, Arc::clone(&answer));
        settle().await;
        assert_eq!(value_held(&held).as_deref(), Some("v2"));
        let revoked = refusal(SecretErrorCode::NotGranted, "revoked");
        *answer.lock().unwrap() = Err(revoked.clone());
        tokio::time::advance(Duration::from_secs(60)).await;
        settle().await;
        assert_eq!(held.current(), Held::Denied(revoked));
        assert_eq!(fetches.load(Ordering::SeqCst), 2);

        // Refused, it asks again with backoff: 1s, then 2s, then 4s.
        tokio::time::advance(Duration::from_secs(1)).await;
        settle().await;
        assert_eq!(fetches.load(Ordering::SeqCst), 3);
        tokio::time::advance(Duration::from_secs(1)).await;
        settle().await;
        assert_eq!(fetches.load(Ordering::SeqCst), 3);
        tokio::time::advance(Duration::from_secs(1)).await;
        settle().await;
        assert_eq!(fetches.load(Ordering::SeqCst), 4);
        // Granted again, the next ask gets the values.
        *answer.lock().unwrap() = Ok("v3".into());
        tokio::time::advance(Duration::from_secs(4)).await;
        settle().await;
        assert_eq!(value_held(&held).as_deref(), Some("v3"));

        // Dropping the handle stops the fetches.
        drop(held);
        tokio::time::advance(Duration::from_secs(600)).await;
        settle().await;
        assert_eq!(fetches.load(Ordering::SeqCst), 5);
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
