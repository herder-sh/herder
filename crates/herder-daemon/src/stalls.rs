//! Stalled agents: sessions that stopped with their work unfinished.
//!
//! With `[follow_ups] stall_after_secs` above 0, a session idle for that long is prompted to
//! report its status and go on, or say it is done, when its work looks unfinished: it has no
//! pull request, or one of its pull requests is open with its checks not running and no pull
//! request follow-up ([`crate::prs`]) waiting to be sent. A session whose pull requests are all
//! merged or closed is done.
//!
//! The prompt is a `user_message` without `by` that carries a [`FollowUp`] with reason
//! `stalled`. A session gets at most `max_stall_nudges` of them between two prompts of a user;
//! a prompt of another agent or a pull request follow-up does not count as a user's. They are
//! counted from the journal, so a restart does not send more.
//!
//! A session is left alone when:
//!
//! - it is not idle: it runs, waits for capacity, needs the user, errored, is archived or moved;
//! - nobody ever prompted it, so it has no work to finish;
//! - it is a child of a task, which its primary session drives;
//! - its last prompt was a stall prompt it answered with a message whose first word is "Done",
//!   as the prompt asks it to start one when it is done.
//!
//! A session running with no new event for `stall_after_secs` is never prompted, as its turn
//! may be a long tool call; it is logged once and listed by [`Stalls::stuck`].

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use anyhow::Result;
use herder_protocol::{
    CiStatus, Event, EventBody, FollowUp, FollowUpReason, ItemBody, PullRequest, Seq, SessionId,
    SessionStatus, Timestamp,
};
use herder_store::Session;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::prs;
use crate::session::journal::Journal;

/// Default for [`Config::interval`].
pub const INTERVAL: Duration = Duration::from_secs(30);

/// The prompt a stalled session gets.
pub const PROMPT: &str = "You stopped before your work looks finished. Report its status in a \
                          sentence or two, then go on with it. If it is done, start your reply \
                          with \"Done\".";

/// What the watcher runs on.
pub struct Config {
    /// How long a session may sit idle with its work unfinished before it is prompted, and run
    /// without an event before it is flagged: `[follow_ups] stall_after_secs`. Zero watches
    /// nothing.
    pub after: Duration,
    /// Most stall prompts between two prompts of a user: `[follow_ups] max_stall_nudges`.
    pub max_nudges: u32,
    /// Whether pull request follow-ups are sent, `[follow_ups] pr_events`; a session owed one
    /// is not stalled.
    pub pr_events: bool,
    /// How often sessions are checked; [`INTERVAL`] outside tests.
    pub interval: Duration,
}

/// Watches every session for stalls. Owned by the session manager.
pub struct Stalls {
    journal: Journal,
    config: Config,
    prompter: prs::Prompter,
    state: Mutex<State>,
    /// One check at a time.
    checking: tokio::sync::Mutex<()>,
}

#[derive(Default)]
struct State {
    sessions: HashMap<SessionId, Watched>,
    /// Sessions running with no event for [`Config::after`].
    stuck: HashSet<SessionId>,
}

/// What a session's journal said at its latest event.
struct Watched {
    last_seq: Seq,
    /// When the latest event was journaled, on the tokio clock.
    last_event: Instant,
    /// When the session last became idle, with the seq of that status change, if it is idle
    /// with its work unfinished and stall prompts left.
    stalls_from: Option<(Seq, Instant)>,
}

impl Stalls {
    /// Starts watching, checking every [`Config::interval`] until `shutdown`.
    pub(crate) fn start(
        journal: Journal,
        config: Config,
        prompter: prs::Prompter,
        shutdown: CancellationToken,
    ) -> Arc<Self> {
        let stalls = Arc::new(Self {
            journal,
            config,
            prompter,
            state: Mutex::new(State::default()),
            checking: tokio::sync::Mutex::new(()),
        });
        if !stalls.config.after.is_zero() {
            tokio::spawn(Arc::clone(&stalls).run(shutdown));
        }
        stalls
    }

    /// Checks every session now: prompts those stalled long enough, flags those stuck.
    pub async fn check(&self) -> Result<()> {
        if self.config.after.is_zero() {
            return Ok(());
        }
        let _check = self.checking.lock().await;
        let now = Instant::now();
        let mut live = HashSet::new();
        for session in self.journal.sessions().await? {
            live.insert(session.session_id.clone());
            self.refresh(&session, now).await?;
            let watched = {
                let state = self.lock();
                state
                    .sessions
                    .get(&session.session_id)
                    .map(|watched| (watched.last_event, watched.stalls_from))
            };
            let Some((last_event, stalls_from)) = watched else {
                continue;
            };
            let id = &session.session_id;
            if session.status == SessionStatus::Running {
                if now.duration_since(last_event) >= self.config.after
                    && self.lock().stuck.insert(id.clone())
                {
                    warn!(session_id = %id, "the agent has been running with no event for {:?}", self.config.after);
                }
                continue;
            }
            self.lock().stuck.remove(id);
            let Some((_, since)) = stalls_from else {
                continue;
            };
            if now.duration_since(since) < self.config.after {
                continue;
            }
            let follow_up = FollowUp {
                reason: FollowUpReason::Stalled,
                pr: None,
                head_sha: None,
            };
            // Not sent: the session got busy since; its next idle stretch counts afresh.
            if (self.prompter)(id.clone(), PROMPT.to_owned(), follow_up).await {
                info!(session_id = %id, "prompted a stalled agent");
            }
        }
        let mut state = self.lock();
        state.sessions.retain(|id, _| live.contains(id));
        state.stuck.retain(|id| live.contains(id));
        Ok(())
    }

    /// Sessions running with no event for `[follow_ups] stall_after_secs`.
    pub fn stuck(&self) -> Vec<SessionId> {
        let mut stuck: Vec<SessionId> = self.lock().stuck.iter().cloned().collect();
        stuck.sort();
        stuck
    }

    async fn run(self: Arc<Self>, shutdown: CancellationToken) {
        loop {
            if let Err(err) = self.check().await {
                warn!("checking for stalled agents failed: {err:#}");
            }
            tokio::select! {
                () = shutdown.cancelled() => return,
                () = tokio::time::sleep(self.config.interval) => {}
            }
        }
    }

    /// Reads the session's journal again if it has new events.
    async fn refresh(&self, session: &Session, now: Instant) -> Result<()> {
        let id = &session.session_id;
        let known = self
            .lock()
            .sessions
            .get(id)
            .map(|watched| (watched.last_seq, watched.stalls_from));
        if known.is_some_and(|(last_seq, _)| last_seq == session.last_seq) {
            return Ok(());
        }
        let stalls_from = if session.status == SessionStatus::Idle && session.parent.is_none() {
            let events = self.journal.all(id.clone()).await?;
            let prs = self.journal.prs(id.clone()).await?;
            let owed = self.config.pr_events && prs::owes_follow_up(&events, &prs);
            let stretch = Stretch::of(&events);
            let stalled = unfinished(&prs, owed)
                && stretch.prompted
                && !stretch.done
                && stretch.nudges < self.config.max_nudges;
            match stretch.idle_since {
                Some((seq, at)) if stalled => {
                    // The same idle stretch keeps the instant it started at.
                    let since = known
                        .and_then(|(_, stalls_from)| stalls_from)
                        .filter(|(known, _)| *known == seq)
                        .map_or_else(|| instant(at, now), |(_, since)| since);
                    Some((seq, since))
                }
                _ => None,
            }
        } else {
            None
        };
        let last_event = instant(session.updated_at, now);
        self.lock().sessions.insert(
            id.clone(),
            Watched {
                last_seq: session.last_seq,
                last_event,
                stalls_from,
            },
        );
        Ok(())
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        // Every update is a single insert, remove or retain, so a poisoned state is still
        // consistent.
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// A session's current stretch of work, as its journal tells it.
#[derive(Debug, Default, PartialEq)]
struct Stretch {
    /// Whether anyone ever prompted the session, other than to say it stalled.
    prompted: bool,
    /// Stall prompts since the latest prompt of a user.
    nudges: u32,
    /// Whether the latest prompt is a stall prompt the agent answered by saying it is done.
    done: bool,
    /// The seq and time of the status change that left the session idle, if it is.
    idle_since: Option<(Seq, Timestamp)>,
}

impl Stretch {
    fn of(events: &[Event]) -> Self {
        let mut stretch = Self::default();
        let mut after_nudge = false;
        for event in events {
            match &event.body {
                EventBody::ItemAdded { item } => match &item.body {
                    ItemBody::UserMessage { .. } => {
                        let stalled = item
                            .follow_up
                            .as_ref()
                            .is_some_and(|follow_up| follow_up.reason == FollowUpReason::Stalled);
                        if stalled {
                            stretch.nudges += 1;
                        } else {
                            stretch.prompted = true;
                            if item.follow_up.is_none() && item.agent_message.is_none() {
                                stretch.nudges = 0;
                            }
                        }
                        after_nudge = stalled;
                        stretch.done = false;
                    }
                    ItemBody::AssistantMessage { text } if after_nudge => {
                        stretch.done = says_done(text);
                    }
                    _ => {}
                },
                EventBody::SessionStatusChanged { status, .. } => {
                    stretch.idle_since =
                        (*status == SessionStatus::Idle).then_some((event.seq, event.at));
                }
                _ => {}
            }
        }
        stretch
    }
}

/// Whether a session with pull requests `prs`, `owed` a pull request follow-up or not, has
/// work left that nothing else will prompt it for.
fn unfinished(prs: &[PullRequest], owed: bool) -> bool {
    if prs.is_empty() {
        return true;
    }
    let checks_running = prs
        .iter()
        .any(|pr| prs::open(pr) && pr.ci == CiStatus::Pending);
    prs.iter().any(prs::open) && !checks_running && !owed
}

/// Whether the first word of `text` is "done", in any case and markup.
fn says_done(text: &str) -> bool {
    text.split(|c: char| !c.is_alphanumeric())
        .find(|word| !word.is_empty())
        .is_some_and(|word| word.eq_ignore_ascii_case("done"))
}

/// `at` on the tokio clock, whose `now` is wall-clock now.
fn instant(at: Timestamp, now: Instant) -> Instant {
    let age = Duration::try_from(Timestamp::now().duration_since(at)).unwrap_or_default();
    now.checked_sub(age).unwrap_or(now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use herder_protocol::{AgentMessage, Item, ItemId, Mergeable, PrState, ReviewStatus, TurnId};

    fn event(seq: Seq, body: EventBody) -> Event {
        Event {
            session_id: SessionId::new("s"),
            seq,
            at: Timestamp::UNIX_EPOCH,
            by: None,
            body,
        }
    }

    fn item(body: ItemBody, follow_up: Option<FollowUpReason>) -> EventBody {
        EventBody::ItemAdded {
            item: Item {
                agent_message: None,
                follow_up: follow_up.map(|reason| FollowUp {
                    reason,
                    pr: None,
                    head_sha: None,
                }),
                parent_call_id: None,
                id: ItemId::new("i"),
                turn_id: TurnId::new("t"),
                body,
            },
        }
    }

    fn prompt(follow_up: Option<FollowUpReason>) -> EventBody {
        item(
            ItemBody::UserMessage {
                text: "Go.".into(),
                attachments: Vec::new(),
            },
            follow_up,
        )
    }

    fn reply(text: &str) -> EventBody {
        item(ItemBody::AssistantMessage { text: text.into() }, None)
    }

    fn idle() -> EventBody {
        EventBody::SessionStatusChanged {
            status: SessionStatus::Idle,
            retry_at: None,
        }
    }

    fn stretch(bodies: Vec<EventBody>) -> Stretch {
        let events: Vec<Event> = bodies
            .into_iter()
            .enumerate()
            .map(|(index, body)| event(index as Seq + 1, body))
            .collect();
        Stretch::of(&events)
    }

    fn pr(number: u64, state: PrState, ci: CiStatus) -> PullRequest {
        PullRequest {
            number,
            url: format!("https://github.com/acme/app/pull/{number}"),
            title: "Fix".into(),
            head_branch: None,
            head_sha: Some("abc".into()),
            unresolved_threads: None,
            state,
            ci,
            review: ReviewStatus::None,
            mergeable: Mergeable::Clean,
        }
    }

    #[test]
    fn work_is_unfinished_without_a_pull_request_or_with_an_open_one_left_alone() {
        assert!(unfinished(&[], false));
        assert!(unfinished(
            &[pr(1, PrState::Open, CiStatus::Passing)],
            false
        ));
        assert!(unfinished(&[pr(1, PrState::Draft, CiStatus::None)], false));
        // A pull request follow-up or its checks will prompt it.
        assert!(!unfinished(
            &[pr(1, PrState::Open, CiStatus::Passing)],
            true
        ));
        assert!(!unfinished(
            &[pr(1, PrState::Open, CiStatus::Pending)],
            false
        ));
        let merged = pr(1, PrState::Merged, CiStatus::Passing);
        assert!(!unfinished(std::slice::from_ref(&merged), false));
        assert!(!unfinished(
            &[pr(1, PrState::Closed, CiStatus::Failing)],
            false
        ));
        assert!(unfinished(
            &[merged, pr(2, PrState::Open, CiStatus::Failing)],
            false
        ));
    }

    #[test]
    fn stall_prompts_count_until_a_user_prompts() {
        let stalled = Some(FollowUpReason::Stalled);
        let got = stretch(vec![prompt(None), idle(), prompt(stalled), idle()]);
        assert_eq!(
            (got.prompted, got.nudges, got.idle_since.map(|(seq, _)| seq)),
            (true, 1, Some(4))
        );
        let pr_follow_up = Some(FollowUpReason::CiFailed);
        let got = stretch(vec![prompt(None), prompt(stalled), prompt(pr_follow_up)]);
        assert_eq!(got.nudges, 1);
        let got = stretch(vec![prompt(None), prompt(stalled), prompt(None)]);
        assert_eq!(got.nudges, 0);
    }

    #[test]
    fn an_agent_prompt_is_work_but_not_a_users() {
        let mut agent = prompt(None);
        if let EventBody::ItemAdded { item } = &mut agent {
            item.agent_message = Some(AgentMessage {
                sender_session_id: SessionId::new("primary"),
                message_id: "m".into(),
                hop_count: 1,
                permission_ceiling: herder_protocol::PermissionMode::Ask,
            });
        }
        let stalled = Some(FollowUpReason::Stalled);
        let got = stretch(vec![prompt(stalled), agent]);
        assert_eq!((got.prompted, got.nudges), (true, 1));
        assert!(!stretch(vec![prompt(stalled)]).prompted);
    }

    #[test]
    fn done_is_the_first_word_of_the_reply_to_a_stall_prompt() {
        let stalled = Some(FollowUpReason::Stalled);
        let done = |text: &str| stretch(vec![prompt(None), prompt(stalled), reply(text)]).done;
        assert!(done("Done. PR #3 is merged."));
        assert!(done("**done** — nothing left"));
        assert!(!done("Not done yet; pushing a fix."));
        assert!(!done("I am done."));
        // A reply to anything else is not an answer to a stall prompt.
        assert!(!stretch(vec![prompt(None), reply("Done.")]).done);
        // Saying done, then working on, ends with a reply that is not.
        let went_on = stretch(vec![
            prompt(None),
            prompt(stalled),
            reply("Done with the tests."),
            reply("Now the docs."),
        ]);
        assert!(!went_on.done);
        // A new prompt starts over.
        assert!(!stretch(vec![prompt(stalled), reply("Done."), prompt(None)]).done);
    }
}
