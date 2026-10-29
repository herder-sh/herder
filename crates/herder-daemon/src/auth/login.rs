//! Logins waiting for approval: a device that holds no passkey, such as a terminal running
//! `herder login`, signs in with `start_login`, shows the code it gets, and waits until an app
//! of a person approves that code with `approve_login`. The device is then that person's.
//!
//! Requests live in memory only and end when their device disconnects, so a restart or a
//! dropped connection voids them.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use herder_protocol::Timestamp;
use ring::rand::{SecureRandom, SystemRandom};
use tokio::sync::oneshot;
use tokio::time::Instant;

use super::{CODE_ALPHABET, normalize};

/// How long a login request waits for its approval.
pub const LOGIN_TTL: Duration = Duration::from_secs(10 * 60);

/// Characters in a login code: 30 bits. Approving one takes a person's passkey, so guessing
/// codes only signs strangers' terminals in as the guesser.
const CODE_LEN: usize = 6;

/// Most requests waiting at once: each comes from a device the daemon does not know yet.
const MAX_PENDING: usize = 32;

/// A device waiting for its login to be approved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Waiting {
    /// SHA-256 of its certificate, as [`super::Auth::authenticate`] knows it.
    pub fingerprint: String,
    /// Its client, from its hello.
    pub client: String,
}

struct Pending {
    waiting: Waiting,
    expires: Instant,
    approved: oneshot::Sender<()>,
}

/// The login requests waiting for approval, by their normalized code.
#[derive(Clone, Default)]
pub struct Logins(Arc<Mutex<HashMap<String, Pending>>>);

/// A started login request, held by the connection of the device that waits.
pub struct Request {
    /// The code to show, `XXX-XXX`.
    pub code: String,
    /// When the request expires.
    pub expires_at: Timestamp,
    approved: oneshot::Receiver<()>,
    expires: Instant,
    key: String,
    logins: Logins,
}

impl Logins {
    /// Starts a login request for the device with certificate `fingerprint`, valid for `ttl`.
    pub fn start(&self, fingerprint: &str, client: &str, ttl: Duration) -> Result<Request> {
        let now = Instant::now();
        let mut pending = self.lock();
        pending.retain(|_, request| request.expires > now && !request.approved.is_closed());
        if pending.len() >= MAX_PENDING {
            bail!("too many logins are waiting for approval; try again in a few minutes");
        }
        let (code, key) = loop {
            let code = new_code()?;
            let key = normalize(&code);
            if !pending.contains_key(&key) {
                break (code, key);
            }
        };
        let (approve, approved) = oneshot::channel();
        let expires = now + ttl;
        pending.insert(
            key.clone(),
            Pending {
                waiting: Waiting {
                    fingerprint: fingerprint.to_owned(),
                    client: client.to_owned(),
                },
                expires,
                approved: approve,
            },
        );
        let expires_at = Timestamp::now()
            .checked_add(ttl)
            .context("the login request would expire too far in the future")?;
        Ok(Request {
            code,
            expires_at,
            approved,
            expires,
            key,
            logins: self.clone(),
        })
    }

    /// The device waiting with `code`, as typed; `None` if no unexpired request has it.
    pub fn waiting(&self, code: &str) -> Option<Waiting> {
        let pending = self.lock();
        let request = pending.get(&normalize(code))?;
        let live = request.expires > Instant::now() && !request.approved.is_closed();
        live.then(|| request.waiting.clone())
    }

    /// Ends the request with `code`, telling its device it was approved; `false` if no
    /// unexpired request has it any more. Call once the device is the approver's.
    pub fn approve(&self, code: &str) -> bool {
        let Some(request) = self.lock().remove(&normalize(code)) else {
            return false;
        };
        request.expires > Instant::now() && request.approved.send(()).is_ok()
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, Pending>> {
        // Every update is a single map operation, so a poisoned map is still consistent.
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Request {
    /// Waits until the request is approved: `true`, or `false` once it expires.
    pub async fn approved(&mut self) -> bool {
        matches!(
            tokio::time::timeout_at(self.expires, &mut self.approved).await,
            Ok(Ok(()))
        )
    }
}

impl Drop for Request {
    /// A device that stops waiting takes its request with it.
    fn drop(&mut self) {
        self.logins.lock().remove(&self.key);
    }
}

/// A random code, formatted `XXX-XXX`.
fn new_code() -> Result<String> {
    let mut bytes = [0u8; CODE_LEN];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| anyhow::anyhow!("the system random number generator failed"))?;
    let chars: String = bytes
        .iter()
        .map(|byte| char::from(CODE_ALPHABET[usize::from(*byte) % CODE_ALPHABET.len()]))
        .collect();
    let (first, second) = chars.split_at(CODE_LEN / 2);
    Ok(format!("{first}-{second}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_two_groups_of_three_crockford_base32() {
        let code = new_code().unwrap();
        assert_eq!(code.len(), 7);
        assert_eq!(&code[3..4], "-");
        assert!(
            code.bytes()
                .filter(|b| *b != b'-')
                .all(|b| CODE_ALPHABET.contains(&b))
        );
    }

    #[tokio::test]
    async fn an_approved_request_wakes_its_device_once() {
        let logins = Logins::default();
        let mut request = logins.start("fp", "herder-cli/1", LOGIN_TTL).unwrap();
        let typed = request.code.to_lowercase().replace('-', " ");
        assert_eq!(
            logins.waiting(&typed),
            Some(Waiting {
                fingerprint: "fp".into(),
                client: "herder-cli/1".into(),
            })
        );

        assert!(logins.approve(&typed));
        assert!(request.approved().await);
        assert!(!logins.approve(&request.code), "a code works once");
        assert_eq!(logins.waiting(&request.code), None);
    }

    #[tokio::test(start_paused = true)]
    async fn an_unapproved_request_expires() {
        let logins = Logins::default();
        let mut request = logins
            .start("fp", "herder-cli/1", Duration::from_secs(5))
            .unwrap();
        assert!(!request.approved().await);
        assert_eq!(logins.waiting(&request.code), None);
        assert!(!logins.approve(&request.code));
    }

    #[test]
    fn a_device_that_stops_waiting_takes_its_request_with_it() {
        let logins = Logins::default();
        let request = logins.start("fp", "herder-cli/1", LOGIN_TTL).unwrap();
        let code = request.code.clone();
        drop(request);
        assert_eq!(logins.waiting(&code), None);
        assert!(!logins.approve(&code));
    }

    #[test]
    fn only_so_many_requests_wait_at_once() {
        let logins = Logins::default();
        let held: Vec<_> = (0..MAX_PENDING)
            .map(|n| logins.start(&format!("fp{n}"), "c", LOGIN_TTL).unwrap())
            .collect();
        assert!(logins.start("one-more", "c", LOGIN_TTL).is_err());
        drop(held);
        assert!(logins.start("one-more", "c", LOGIN_TTL).is_ok());
    }
}
