//! What a person's devices settle for each other.
//!
//! A device without a passkey, such as a terminal signed in with `herder login`, asks for
//! something a person does only with a fresh assertion: the daemon sends an
//! `approval_request` to the person's connections that sign assertions, and holds the command
//! until one of them approves it with an assertion, refuses it, or [`APPROVAL_TTL`] passes.
//!
//! It also remembers the terminals each device opened or attached since the daemon started,
//! which that device re-attaches, after a reconnect, without an assertion.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use herder_protocol::{
    ApprovalRequest, DeviceId, ErrorCode, ErrorInfo, ServerMessage, TerminalId, Timestamp, UserId,
};
use tokio::sync::oneshot;

use crate::hub::Outbox;

/// How long a device waits for its person to approve.
pub(crate) const APPROVAL_TTL: Duration = Duration::from_secs(120);

/// The open requests, the connections that may settle them, and each device's terminals.
pub(crate) struct Approvals {
    ttl: Duration,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    /// Open requests by id.
    open: HashMap<String, Open>,
    /// Connections that sign assertions, with the person each acts as.
    signers: Vec<(UserId, Arc<Outbox>)>,
    /// The terminals each device opened or attached.
    terminals: HashMap<DeviceId, HashSet<TerminalId>>,
}

struct Open {
    /// The person who asked, who alone settles it.
    user_id: UserId,
    request: ApprovalRequest,
    settled: oneshot::Sender<Result<(), ErrorInfo>>,
}

/// A request waiting for its person; see [`Approvals::wait`].
pub(crate) struct Waiting {
    request_id: String,
    settled: oneshot::Receiver<Result<(), ErrorInfo>>,
}

impl Approvals {
    /// Requests that wait `ttl` for their person.
    pub(crate) fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            state: Mutex::default(),
        }
    }

    /// Notes a connection of `user_id`; one that signs assertions gets the open requests of
    /// its person, then each new one, until [`Approvals::disconnect`].
    pub(crate) fn connect(&self, user_id: &UserId, outbox: &Arc<Outbox>, signs: bool) {
        if !signs {
            return;
        }
        let mut state = self.lock();
        outbox.push_all(
            state
                .open
                .values()
                .filter(|open| &open.user_id == user_id)
                .map(|open| ServerMessage::ApprovalRequest(open.request.clone())),
        );
        state.signers.push((user_id.clone(), Arc::clone(outbox)));
    }

    /// Forgets a connection.
    pub(crate) fn disconnect(&self, outbox: &Arc<Outbox>) {
        self.lock()
            .signers
            .retain(|(_, signer)| !Arc::ptr_eq(signer, outbox));
    }

    /// Asks `user_id`'s connections that sign assertions to approve `action`, which
    /// `client` asks for. Refused when none is connected, as nobody could approve.
    pub(crate) fn request(
        &self,
        user_id: &UserId,
        action: String,
        client: &str,
    ) -> Result<Waiting, ErrorInfo> {
        let mut state = self.lock();
        let signers: Vec<Arc<Outbox>> = state
            .signers
            .iter()
            .filter(|(signer, _)| signer == user_id)
            .map(|(_, outbox)| Arc::clone(outbox))
            .collect();
        if signers.is_empty() {
            return Err(ErrorInfo {
                code: ErrorCode::Forbidden,
                message: format!(
                    "{action} needs your passkey: open a herder app signed in as you to \
                     approve it there"
                ),
            });
        }
        let expires_at = Timestamp::now()
            .checked_add(self.ttl)
            .map_err(|err| ErrorInfo {
                code: ErrorCode::Internal,
                message: format!("{err}"),
            })?;
        let request = ApprovalRequest {
            request_id: ulid::Ulid::generate().to_string(),
            action,
            client: client.to_owned(),
            expires_at,
        };
        for outbox in &signers {
            outbox.push(ServerMessage::ApprovalRequest(request.clone()));
        }
        let (settled, waiting) = oneshot::channel();
        let request_id = request.request_id.clone();
        state.open.insert(
            request_id.clone(),
            Open {
                user_id: user_id.clone(),
                request,
                settled,
            },
        );
        Ok(Waiting {
            request_id,
            settled: waiting,
        })
    }

    /// Waits until `waiting` is approved, refused or expired.
    pub(crate) async fn wait(&self, waiting: Waiting) -> Result<(), ErrorInfo> {
        match tokio::time::timeout(self.ttl, waiting.settled).await {
            Ok(Ok(settled)) => settled,
            _ => {
                self.close(&waiting.request_id);
                Err(ErrorInfo {
                    code: ErrorCode::Forbidden,
                    message: "nobody approved it in time; approve it in a herder app signed in \
                              as you"
                        .to_owned(),
                })
            }
        }
    }

    /// Settles `request_id` for `user_id`, who must be the person who asked: approved, or
    /// refused.
    pub(crate) fn settle(
        &self,
        user_id: &UserId,
        request_id: &str,
        approved: bool,
    ) -> Result<(), ErrorInfo> {
        let open = {
            let mut state = self.lock();
            match state.open.get(request_id) {
                Some(open) if &open.user_id == user_id => state.open.remove(request_id),
                _ => None,
            }
        };
        let Some(open) = open else {
            return Err(ErrorInfo {
                code: ErrorCode::NotFound,
                message: "that request is closed: approved, refused or expired".to_owned(),
            });
        };
        self.closed(&open);
        let settled = if approved {
            Ok(())
        } else {
            Err(ErrorInfo {
                code: ErrorCode::Forbidden,
                message: format!(
                    "{} was refused on another of your devices",
                    open.request.action
                ),
            })
        };
        // The device stopped waiting if it is gone; there is nothing left to tell it.
        let _ = open.settled.send(settled);
        Ok(())
    }

    /// Closes `request_id` without settling it.
    fn close(&self, request_id: &str) {
        let open = self.lock().open.remove(request_id);
        if let Some(open) = open {
            self.closed(&open);
        }
    }

    /// Tells the person's signing connections that `open` is closed.
    fn closed(&self, open: &Open) {
        let state = self.lock();
        for (_, outbox) in state
            .signers
            .iter()
            .filter(|(signer, _)| signer == &open.user_id)
        {
            outbox.push(ServerMessage::ApprovalClosed {
                request_id: open.request.request_id.clone(),
            });
        }
    }

    /// Notes that `device_id` opened or attached `terminal_id`.
    pub(crate) fn attached(&self, device_id: &DeviceId, terminal_id: &TerminalId) {
        self.lock()
            .terminals
            .entry(device_id.clone())
            .or_default()
            .insert(terminal_id.clone());
    }

    /// Whether `device_id` opened or attached `terminal_id` since the daemon started.
    pub(crate) fn had(&self, device_id: &DeviceId, terminal_id: &TerminalId) -> bool {
        self.lock()
            .terminals
            .get(device_id)
            .is_some_and(|terminals| terminals.contains(terminal_id))
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        // Every update is a single insert or remove, so a poisoned state is still consistent.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use herder_protocol::{ErrorCode, ServerMessage, UserId};

    use super::Approvals;
    use crate::hub::Outbox;

    fn drain(outbox: &Outbox) -> Vec<ServerMessage> {
        std::iter::from_fn(|| outbox.pop()).collect()
    }

    #[tokio::test]
    async fn a_request_is_approved_refused_or_expires() {
        let approvals = Arc::new(Approvals::new(Duration::from_secs(60)));
        let tomas = UserId::new("tomas");
        let phone = Arc::new(Outbox::default());
        let tui = Arc::new(Outbox::default());
        approvals.connect(&tomas, &phone, true);
        approvals.connect(&tomas, &tui, false);

        let waiting = approvals
            .request(&tomas, "Open a terminal".into(), "herder-tui")
            .unwrap();
        let [ServerMessage::ApprovalRequest(request)] = &drain(&phone)[..] else {
            panic!("the phone was not asked");
        };
        assert!(
            drain(&tui).is_empty(),
            "a device without a passkey is never asked"
        );
        // Only the person who asked settles it.
        let sam = UserId::new("sam");
        assert_eq!(
            approvals
                .settle(&sam, &request.request_id, true)
                .unwrap_err()
                .code,
            ErrorCode::NotFound
        );
        approvals.settle(&tomas, &request.request_id, true).unwrap();
        approvals.wait(waiting).await.unwrap();
        assert!(matches!(
            &drain(&phone)[..],
            [ServerMessage::ApprovalClosed { .. }]
        ));

        let waiting = approvals
            .request(&tomas, "Open a terminal".into(), "herder-tui")
            .unwrap();
        let [ServerMessage::ApprovalRequest(request)] = &drain(&phone)[..] else {
            panic!("the phone was not asked");
        };
        approvals
            .settle(&tomas, &request.request_id, false)
            .unwrap();
        let refused = approvals.wait(waiting).await.unwrap_err();
        assert!(refused.message.contains("refused"), "{}", refused.message);

        // A phone that connects later is told of what is open.
        let short = Approvals::new(Duration::ZERO);
        short.connect(&tomas, &phone, true);
        let waiting = short
            .request(&tomas, "Open a terminal".into(), "herder-tui")
            .unwrap();
        let later = Arc::new(Outbox::default());
        short.connect(&tomas, &later, true);
        assert_eq!(drain(&later).len(), 1);
        let expired = short.wait(waiting).await.unwrap_err();
        assert!(expired.message.contains("in time"), "{}", expired.message);
        assert!(matches!(
            drain(&later).last(),
            Some(ServerMessage::ApprovalClosed { .. })
        ));

        // With no phone to ask, it is refused at once.
        approvals.disconnect(&phone);
        assert!(
            approvals
                .request(&tomas, "Open a terminal".into(), "herder-tui")
                .is_err()
        );
    }
}
