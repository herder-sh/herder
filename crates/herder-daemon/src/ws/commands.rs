//! Command idempotency: a resend with the same id is answered without being applied again.
//!
//! This in-memory log covers a connection that drops while its command runs; the backend
//! remembers accepted session commands across restarts too ([`super::Backend::command`]).

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::sync::{Arc, Mutex};

use herder_protocol::{CommandId, CommandResult, ErrorCode, ErrorInfo, UserId};
use tokio::sync::OnceCell;

/// Accepted commands remembered per daemon; the oldest are forgotten first. A client resends
/// after a reconnect, so only recent ids matter.
const REMEMBERED: usize = 4096;

type Answer = Result<CommandResult, ErrorInfo>;
type Key = (UserId, CommandId);

/// Answers of recent commands, by user and command id.
#[derive(Debug, Default)]
pub(super) struct Commands {
    log: Mutex<Log>,
}

#[derive(Debug, Default)]
struct Log {
    /// Each answer with the number it was inserted as, so eviction skips re-inserted keys.
    answers: HashMap<Key, (u64, Arc<OnceCell<Answer>>)>,
    order: VecDeque<(u64, Key)>,
    inserted: u64,
}

impl Commands {
    /// Runs `apply` unless `key` was already accepted (or is running), and returns its answer.
    ///
    /// `apply` runs as its own task, so a client that disconnects mid-command cannot leave it
    /// half-applied and retried. A rejected command changed nothing and is forgotten, so a
    /// resend is tried afresh.
    pub(super) async fn apply(
        &self,
        key: Key,
        apply: impl Future<Output = Answer> + Send + 'static,
    ) -> Answer {
        let answer = self.answer(&key);
        let result = answer
            .get_or_init(|| async move {
                tokio::spawn(apply).await.unwrap_or_else(|_| {
                    Err(ErrorInfo {
                        code: ErrorCode::Internal,
                        message: "the command failed".to_owned(),
                    })
                })
            })
            .await
            .clone();
        if result.is_err() {
            let mut log = self.lock();
            if log
                .answers
                .get(&key)
                .is_some_and(|(_, cell)| Arc::ptr_eq(cell, &answer))
            {
                log.answers.remove(&key);
            }
        }
        result
    }

    fn answer(&self, key: &Key) -> Arc<OnceCell<Answer>> {
        let mut log = self.lock();
        if let Some((_, answer)) = log.answers.get(key) {
            return Arc::clone(answer);
        }
        log.inserted += 1;
        let number = log.inserted;
        let answer = Arc::new(OnceCell::new());
        log.answers
            .insert(key.clone(), (number, Arc::clone(&answer)));
        log.order.push_back((number, key.clone()));
        while log.order.len() > REMEMBERED {
            let Some((number, old)) = log.order.pop_front() else {
                break;
            };
            if log.answers.get(&old).is_some_and(|(n, _)| *n == number) {
                log.answers.remove(&old);
            }
        }
        answer
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Log> {
        // Every update is a single insert or remove, so a poisoned log is still consistent.
        self.log
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    fn key(id: &str) -> Key {
        (UserId::new("u"), CommandId::new(id))
    }

    #[tokio::test]
    async fn an_accepted_command_is_applied_once() {
        let commands = Commands::default();
        let runs = Arc::new(AtomicUsize::new(0));
        for _ in 0..3 {
            let runs = Arc::clone(&runs);
            let answer = commands
                .apply(key("c1"), async move {
                    runs.fetch_add(1, Ordering::SeqCst);
                    Ok(CommandResult::Applied)
                })
                .await;
            assert_eq!(answer, Ok(CommandResult::Applied));
        }
        assert_eq!(runs.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_rejected_command_is_tried_again() {
        let commands = Commands::default();
        let rejected = Err(ErrorInfo {
            code: ErrorCode::Conflict,
            message: "busy".into(),
        });
        let answer = commands.apply(key("c1"), {
            let rejected = rejected.clone();
            async move { rejected }
        });
        assert_eq!(answer.await, rejected);
        let answer = commands.apply(key("c1"), async { Ok(CommandResult::Applied) });
        assert_eq!(answer.await, Ok(CommandResult::Applied));
    }

    #[tokio::test]
    async fn the_oldest_answers_are_forgotten() {
        let commands = Commands::default();
        for n in 0..=REMEMBERED {
            let answer = commands.apply(key(&n.to_string()), async { Ok(CommandResult::Applied) });
            answer.await.unwrap();
        }
        assert_eq!(commands.lock().answers.len(), REMEMBERED);
        assert!(!commands.lock().answers.contains_key(&key("0")));
    }
}
