//! Pull requests: the keys and reducer of the open session's PR strip, the cross-session PR
//! view, and the prompt that links a PR by number or URL. Drawing is in `views::prs`.

use herder_protocol::{CommandBody, PullRequest};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::action::Action;
use crate::app::{App, Effect, Focus, Row};
use crate::compose::Origin;
use crate::session::SessionKey;

/// What the PR keys ask for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrAction {
    /// Focus the PR strip of the open session, opening the selected session first.
    FocusStrip,
    /// Show or hide every session's PRs in the main pane.
    ToggleAll,
    /// Move the PR selection by this many rows.
    Move(isize),
    /// Select the first PR.
    First,
    /// Select the last PR.
    Last,
    /// Open the selected PR in the browser.
    Browse,
    /// Open the selected PR's session.
    OpenSession,
    /// Stop tracking the selected PR for its session.
    Unlink,
    /// Leave the PR list: back to the transcript, else to the session list.
    Close,
    /// Ask for a PR to link to the targeted session.
    StartLink,
    /// Type a character into the link prompt.
    Type(char),
    /// Delete the link prompt's last character.
    Erase,
    /// Link the PR the prompt names.
    Submit,
    /// Close the link prompt.
    Cancel,
}

/// The PR UI's own state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Prs {
    /// Selected row of the open session's strip.
    pub strip: usize,
    /// Selected row of the cross-session view.
    pub all: usize,
    /// The link prompt, while it is open.
    pub prompt: Option<Prompt>,
}

/// The link prompt: which session it links to, and what was typed so far.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prompt {
    /// The session the PR is linked to.
    pub key: SessionKey,
    /// Text typed so far.
    pub text: String,
}

/// The PR action a key asks for, if the PR UI handles it in the app's current state.
pub fn for_key(key: KeyEvent, app: &App) -> Option<PrAction> {
    if app.prs.prompt.is_some() {
        return match key.code {
            KeyCode::Enter => Some(PrAction::Submit),
            KeyCode::Esc => Some(PrAction::Cancel),
            KeyCode::Backspace if app.prs.prompt.as_ref().is_some_and(|p| p.text.is_empty()) => {
                Some(PrAction::Cancel)
            }
            KeyCode::Backspace => Some(PrAction::Erase),
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                Some(PrAction::Type(c))
            }
            _ => None,
        };
    }
    let list = matches!(app.focus, Focus::Prs | Focus::AllPrs);
    let action = match key.code {
        KeyCode::Char('P') => PrAction::ToggleAll,
        KeyCode::Char('p') => PrAction::FocusStrip,
        KeyCode::Char('L') => PrAction::StartLink,
        KeyCode::Char('k') | KeyCode::Up if list => PrAction::Move(-1),
        KeyCode::Char('j') | KeyCode::Down if list => PrAction::Move(1),
        KeyCode::PageUp if list => PrAction::Move(-10),
        KeyCode::PageDown if list => PrAction::Move(10),
        KeyCode::Char('g') | KeyCode::Home if list => PrAction::First,
        KeyCode::Char('G') | KeyCode::End if list => PrAction::Last,
        KeyCode::Enter | KeyCode::Char('o') if list => PrAction::Browse,
        KeyCode::Char('x') if list => PrAction::Unlink,
        KeyCode::Char('l') | KeyCode::Right if app.focus == Focus::AllPrs => PrAction::OpenSession,
        // Back to the transcript from the strip, out of the view from the cross-session one.
        KeyCode::Esc | KeyCode::Backspace | KeyCode::Char('h') | KeyCode::Left if list => {
            PrAction::Close
        }
        _ => return None,
    };
    Some(action)
}

/// The PR number `text` names: `123`, `#123`, or a pull request URL.
pub fn parse_number(text: &str) -> Option<u64> {
    let text = text.trim();
    let digits = match text.split_once("/pull/") {
        Some((_, rest)) => rest.split(['/', '#', '?']).next().unwrap_or(rest),
        None => text.strip_prefix('#').unwrap_or(text),
    };
    digits.parse().ok().filter(|number| *number > 0)
}

impl App {
    /// Carries out one PR action.
    pub fn act_pr(&mut self, action: PrAction) -> Vec<Effect> {
        match action {
            PrAction::FocusStrip => {
                if self.focus == Focus::Sessions || self.focus == Focus::AllPrs {
                    self.act(Action::Open);
                }
                match self.open_session() {
                    Some(session) if !session.prs.is_empty() => self.focus = Focus::Prs,
                    Some(_) => self.notice = Some("no pull requests linked; L links one".into()),
                    None => {}
                }
            }
            PrAction::ToggleAll => {
                self.focus = match self.focus {
                    Focus::AllPrs if self.open.is_some() => Focus::Transcript,
                    Focus::AllPrs => Focus::Sessions,
                    _ => Focus::AllPrs,
                };
            }
            PrAction::Close => {
                self.focus = if self.open.is_some() {
                    Focus::Transcript
                } else {
                    Focus::Sessions
                };
            }
            PrAction::Move(step) => {
                let len = self.pr_rows().len();
                let at = self.pr_index();
                *self.pr_cursor() = at.saturating_add_signed(step).min(len.saturating_sub(1));
            }
            PrAction::First => *self.pr_cursor() = 0,
            PrAction::Last => *self.pr_cursor() = self.pr_rows().len().saturating_sub(1),
            PrAction::Browse => {
                if let Some((_, pr)) = self.selected_pr() {
                    return vec![Effect::OpenUrl(pr.url.clone())];
                }
            }
            PrAction::OpenSession => {
                if let Some((key, _)) = self.selected_pr() {
                    let key = key.clone();
                    let row = Row::Session { key, depth: 0 };
                    self.choose_row(row);
                    self.act(Action::Open);
                    self.prs.strip = 0;
                    self.focus = Focus::Prs;
                }
            }
            PrAction::Unlink => {
                if let Some((key, pr)) = self.selected_pr() {
                    let command = CommandBody::UnlinkPr {
                        session_id: key.session_id.clone(),
                        number: pr.number,
                    };
                    return vec![Effect::Send {
                        host_id: key.host_id.clone(),
                        command,
                        origin: Origin::Session(key.clone()),
                    }];
                }
            }
            PrAction::StartLink => match self.pr_target() {
                Some(key) => {
                    self.prs.prompt = Some(Prompt {
                        key,
                        text: String::new(),
                    });
                }
                None => self.notice = Some("select a session to link a pull request to".into()),
            },
            PrAction::Type(c) => {
                if let Some(prompt) = &mut self.prs.prompt {
                    prompt.text.push(c);
                }
            }
            PrAction::Erase => {
                if let Some(prompt) = &mut self.prs.prompt {
                    prompt.text.pop();
                }
            }
            PrAction::Cancel => self.prs.prompt = None,
            PrAction::Submit => {
                let Some(prompt) = self.prs.prompt.take() else {
                    return Vec::new();
                };
                let Some(number) = parse_number(&prompt.text) else {
                    self.notice = Some(format!(
                        "not a pull request number or URL: {}",
                        prompt.text.trim()
                    ));
                    self.prs.prompt = Some(prompt);
                    return Vec::new();
                };
                let command = CommandBody::LinkPr {
                    session_id: prompt.key.session_id.clone(),
                    number,
                };
                return vec![Effect::Send {
                    host_id: prompt.key.host_id.clone(),
                    command,
                    origin: Origin::Session(prompt.key),
                }];
            }
        }
        Vec::new()
    }

    /// Every linked PR of every listed session, in session-list order: what the cross-session
    /// view lists.
    pub fn all_prs(&self) -> Vec<(&SessionKey, &PullRequest)> {
        self.all_rows()
            .iter()
            .filter_map(|row| match row {
                Row::Session { key, .. } => self.sessions.get_key_value(key),
                Row::Machine(_) | Row::Host { .. } | Row::Project(_) => None,
            })
            .flat_map(|(key, session)| session.prs.iter().map(move |pr| (key, pr)))
            .collect()
    }

    /// The open session's linked PRs, as its strip lists them.
    pub fn strip_prs(&self) -> Vec<(&SessionKey, &PullRequest)> {
        match (&self.open, self.open_session()) {
            (Some(key), Some(session)) => session.prs.iter().map(|pr| (key, pr)).collect(),
            _ => Vec::new(),
        }
    }

    /// Index of the selected row of the PR list that has focus, kept within the list.
    pub fn pr_index(&self) -> usize {
        let at = if self.focus == Focus::AllPrs {
            self.prs.all
        } else {
            self.prs.strip
        };
        at.min(self.pr_rows().len().saturating_sub(1))
    }

    fn pr_rows(&self) -> Vec<(&SessionKey, &PullRequest)> {
        if self.focus == Focus::AllPrs {
            self.all_prs()
        } else {
            self.strip_prs()
        }
    }

    fn pr_cursor(&mut self) -> &mut usize {
        if self.focus == Focus::AllPrs {
            &mut self.prs.all
        } else {
            &mut self.prs.strip
        }
    }

    fn selected_pr(&self) -> Option<(&SessionKey, &PullRequest)> {
        if !matches!(self.focus, Focus::Prs | Focus::AllPrs) {
            return None;
        }
        self.pr_rows().into_iter().nth(self.pr_index())
    }

    /// The session `L` links to: the selected one in the session list, the selected PR's in
    /// the cross-session view, else the open one.
    fn pr_target(&self) -> Option<SessionKey> {
        match self.focus {
            Focus::Sessions => self.selected().and_then(|row| match row {
                Row::Session { key, .. } => Some(key),
                Row::Machine(_) | Row::Host { .. } | Row::Project(_) => None,
            }),
            Focus::AllPrs => self.selected_pr().map(|(key, _)| key.clone()),
            _ => self.open.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use herder_protocol::{
        CiStatus, EventBody, HostId, Mergeable, PrState, ReviewStatus, SessionId,
    };
    use ratatui::crossterm::event::KeyModifiers;

    use super::*;
    use crate::app::Msg;
    use crate::fake::{self, key, pr, update};

    fn press(app: &mut App, code: KeyCode) -> Vec<Effect> {
        app.update(Msg::Key(KeyEvent::new(code, KeyModifiers::NONE)))
    }

    fn typed(app: &mut App, text: &str) {
        for c in text.chars() {
            press(app, KeyCode::Char(c));
        }
    }

    #[test]
    fn numbers_and_urls_name_a_pull_request() {
        assert_eq!(parse_number("42"), Some(42));
        assert_eq!(parse_number(" #42 "), Some(42));
        assert_eq!(
            parse_number("https://github.com/acme/app/pull/42/files#diff"),
            Some(42)
        );
        assert_eq!(
            parse_number("https://github.com/acme/app/pull/42?x=1"),
            Some(42)
        );
        assert_eq!(parse_number("github.com/acme/app/issues/42"), None);
        assert_eq!(parse_number("0"), None);
        assert_eq!(parse_number(""), None);
    }

    /// The P1.9 scenarios as their events: each lands on its own session only.
    #[test]
    fn pr_events_track_each_sessions_prs() {
        let mut app = fake::tree();
        let mut opened = pr(7, "Add health endpoint", PrState::Open);
        opened.ci = CiStatus::Pending;
        fake::feed(
            &mut app,
            "h1",
            "s2",
            update(
                "s2",
                3,
                vec![
                    EventBody::PrLinked { pr: opened.clone() },
                    EventBody::PrLinked {
                        pr: pr(9, "Docs", PrState::Draft),
                    },
                ],
                vec![],
            ),
        );
        // CI passes, then a review approves, then it conflicts, then it merges.
        let mut updated = opened.clone();
        for change in [
            |pr: &mut PullRequest| pr.ci = CiStatus::Passing,
            |pr: &mut PullRequest| pr.review = ReviewStatus::Approved,
            |pr: &mut PullRequest| pr.mergeable = Mergeable::Conflicting,
            |pr: &mut PullRequest| pr.state = PrState::Merged,
        ] {
            change(&mut updated);
            fake::feed(
                &mut app,
                "h1",
                "s2",
                update(
                    "s2",
                    5,
                    vec![EventBody::PrUpdated {
                        pr: updated.clone(),
                    }],
                    vec![],
                ),
            );
        }
        let s2 = &app.sessions[&key("h1", "s2")];
        assert_eq!(s2.prs.len(), 2);
        assert_eq!(s2.prs[0], updated);
        assert!(app.sessions[&key("h1", "s1")].prs.is_empty());

        fake::feed(
            &mut app,
            "h1",
            "s2",
            update("s2", 9, vec![EventBody::PrUnlinked { number: 7 }], vec![]),
        );
        let numbers: Vec<u64> = app.sessions[&key("h1", "s2")]
            .prs
            .iter()
            .map(|pr| pr.number)
            .collect();
        assert_eq!(numbers, [9]);
    }

    #[test]
    fn the_strip_selects_opens_and_unlinks() {
        let mut app = fake::with_prs();
        press(&mut app, KeyCode::Char('p'));
        assert_eq!(app.open, Some(key("h1", "s2")));
        assert_eq!(app.focus, Focus::Prs);
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            [Effect::OpenUrl(
                "https://github.com/acme/app/pull/7".to_owned()
            )]
        );
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.pr_index(), 1, "the selection stops at the last PR");
        assert_eq!(
            press(&mut app, KeyCode::Char('x')),
            [Effect::Send {
                host_id: HostId::new("h1"),
                command: CommandBody::UnlinkPr {
                    session_id: SessionId::new("s2"),
                    number: 9,
                },
                origin: Origin::Session(key("h1", "s2")),
            }]
        );
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.focus, Focus::Transcript);
        // Out of the strip, Enter and x mean nothing to it.
        assert_eq!(press(&mut app, KeyCode::Char('x')), []);
    }

    #[test]
    fn a_session_without_prs_says_how_to_link_one() {
        let mut app = fake::with_prs();
        press(&mut app, KeyCode::Char('G'));
        press(&mut app, KeyCode::Char('p'));
        assert_eq!(app.open, Some(key("h1", "s1")));
        assert_eq!(app.focus, Focus::Transcript);
        assert!(app.notice.as_deref().unwrap().contains("L links one"));
        // The notice goes with the next key.
        press(&mut app, KeyCode::Char('k'));
        assert_eq!(app.notice, None);
    }

    #[test]
    fn the_prompt_links_by_number_or_url_to_the_selected_session() {
        let mut app = fake::with_prs();
        press(&mut app, KeyCode::Char('G'));
        press(&mut app, KeyCode::Char('L'));
        assert_eq!(app.prs.prompt.as_ref().unwrap().key, key("h1", "s1"));
        // Keys type into the prompt instead of acting.
        typed(&mut app, "qx#12");
        press(&mut app, KeyCode::Backspace);
        assert_eq!(app.prs.prompt.as_ref().unwrap().text, "qx#1");
        assert_eq!(press(&mut app, KeyCode::Enter), []);
        assert!(app.notice.as_deref().unwrap().contains("qx#1"));
        assert!(app.prs.prompt.is_some(), "a bad entry keeps the prompt");

        press(&mut app, KeyCode::Esc);
        assert_eq!(app.prs.prompt, None);
        press(&mut app, KeyCode::Char('L'));
        typed(&mut app, "https://github.com/acme/app/pull/31");
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            [Effect::Send {
                host_id: HostId::new("h1"),
                command: CommandBody::LinkPr {
                    session_id: SessionId::new("s1"),
                    number: 31,
                },
                origin: Origin::Session(key("h1", "s1")),
            }]
        );
        assert_eq!(app.prs.prompt, None);
    }

    #[test]
    fn the_cross_session_view_lists_every_pr_and_jumps_to_its_session() {
        let mut app = fake::with_prs();
        press(&mut app, KeyCode::Char('P'));
        assert_eq!(app.focus, Focus::AllPrs);
        let all: Vec<(SessionKey, u64)> = app
            .all_prs()
            .into_iter()
            .map(|(key, pr)| (key.clone(), pr.number))
            .collect();
        assert_eq!(
            all,
            [
                (key("h1", "s2"), 7),
                (key("h1", "s2"), 9),
                (key("h1", "s3"), 8)
            ]
        );
        press(&mut app, KeyCode::Char('G'));
        // L there links to the selected PR's session.
        press(&mut app, KeyCode::Char('L'));
        assert_eq!(app.prs.prompt.as_ref().unwrap().key, key("h1", "s3"));
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('l'));
        assert_eq!(app.open, Some(key("h1", "s3")));
        assert_eq!(app.focus, Focus::Prs);
        assert_eq!(
            app.selected(),
            Some(Row::Session {
                key: key("h1", "s3"),
                depth: 1
            })
        );

        press(&mut app, KeyCode::Char('P'));
        press(&mut app, KeyCode::Char('P'));
        assert_eq!(app.focus, Focus::Transcript);
    }

    #[test]
    fn a_refused_link_for_a_session_not_in_view_shows_on_the_status_line() {
        let mut app = fake::with_prs();
        press(&mut app, KeyCode::Char('P'));
        app.update(Msg::Sent {
            origin: Origin::Session(key("h1", "s1")),
            result: Err("pull request #4 does not exist in acme/app".into()),
        });
        assert_eq!(
            app.notice.as_deref(),
            Some("pull request #4 does not exist in acme/app")
        );
    }

    #[test]
    fn pr_keys_type_into_the_composer() {
        let mut app = fake::with_prs();
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Char('i'));
        typed(&mut app, "pPLx");
        assert_eq!(app.focus, Focus::Composer);
        assert_eq!(app.compose.editor.lines(), ["pPLx"]);
        assert_eq!(app.prs.prompt, None);
    }
}
