//! The accounts screen: every machine's accounts with their usage windows, the add-account
//! dialog for the selected machine, and logging the selected account in again once its login
//! expired, in a login terminal like adding one.
//!
//! Usage is what the daemon last heard from each provider. Every account takes part in
//! failover; pinning is set in each daemon's own config, and the screen shows it as the daemon
//! reports it and says where it lives.

use herder_client_core::Machine;
use herder_protocol::{Account, AccountId, HostId};
use ratatui::crossterm::event::{KeyCode, KeyEvent};

use crate::accounts::{self, AddAccount, Outcome};
use crate::action::Action;
use crate::app::{App, Effect};
use crate::terminal::{self, Target};

/// The accounts screen, shown over the main screen.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AccountScreen {
    /// The row the user selected; until they move, the first one.
    pub chosen: Option<Pick>,
    /// The add-account dialog, over the screen.
    pub adding: Option<AddAccount>,
}

/// A row of the screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pick {
    /// A machine's heading.
    Machine(HostId),
    /// One of its accounts.
    Account(HostId, AccountId),
}

impl Pick {
    /// The machine the row belongs to.
    pub fn host_id(&self) -> &HostId {
        match self {
            Pick::Machine(host_id) | Pick::Account(host_id, _) => host_id,
        }
    }
}

/// Every row: each machine, then its accounts in the daemon's order.
pub fn rows(machines: &[Machine]) -> Vec<Pick> {
    machines
        .iter()
        .flat_map(|machine| {
            let host_id = &machine.host_id;
            std::iter::once(Pick::Machine(host_id.clone())).chain(
                machine
                    .accounts
                    .iter()
                    .map(|account| Pick::Account(host_id.clone(), account.account_id.clone())),
            )
        })
        .collect()
}

impl AccountScreen {
    /// Index of the selected row in `rows`: the chosen one, else the first account, else the
    /// first machine.
    pub fn selected(&self, rows: &[Pick]) -> Option<usize> {
        match &self.chosen {
            Some(chosen) => rows.iter().position(|row| row == chosen),
            None => rows
                .iter()
                .position(|row| matches!(row, Pick::Account(..)))
                .or((!rows.is_empty()).then_some(0)),
        }
    }
}

/// Input to the screen or its dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input {
    /// Close the screen.
    Close,
    /// Select the previous row.
    Up,
    /// Select the next row.
    Down,
    /// Select the first row.
    Top,
    /// Select the last row.
    Bottom,
    /// Open the add-account dialog for the selected row's machine.
    Add,
    /// Log the selected account in again.
    LogInAgain,
    /// Input to the add-account dialog.
    Dialog(crate::machines::Input),
}

/// The action a key asks for while the screen is open.
pub fn for_key(key: KeyEvent, screen: &AccountScreen) -> Option<Action> {
    if screen.adding.is_some() {
        return accounts::input_for_key(key).map(|input| Action::Accounts(Input::Dialog(input)));
    }
    let input = match key.code {
        KeyCode::Esc | KeyCode::Backspace | KeyCode::Left | KeyCode::Char('A' | 'h' | 'q') => {
            Input::Close
        }
        KeyCode::Char('k') | KeyCode::Up => Input::Up,
        KeyCode::Char('j') | KeyCode::Down => Input::Down,
        KeyCode::Char('g') | KeyCode::Home | KeyCode::PageUp => Input::Top,
        KeyCode::Char('G') | KeyCode::End | KeyCode::PageDown => Input::Bottom,
        KeyCode::Char('n') => Input::Add,
        KeyCode::Char('l') => Input::LogInAgain,
        KeyCode::Char('r') => return Some(Action::Reconnect),
        KeyCode::Char('?') => return Some(Action::ToggleHelp),
        _ => return None,
    };
    Some(Action::Accounts(input))
}

impl App {
    /// Carries out one input to the accounts screen.
    pub(crate) fn account_input(&mut self, input: Input) -> Vec<Effect> {
        let Some(screen) = &mut self.account_screen else {
            return Vec::new();
        };
        if let Input::Dialog(input) = input {
            let Some(adding) = &mut screen.adding else {
                return Vec::new();
            };
            match adding.input(input) {
                Outcome::Open => {}
                Outcome::Closed => screen.adding = None,
                Outcome::Login(new) => {
                    let host_id = adding.host_id.clone();
                    let target = terminal::login_or_install(&self.machines, &host_id, new);
                    screen.adding = None;
                    return vec![Effect::AttachTerminal { host_id, target }];
                }
            }
            return Vec::new();
        }
        let rows = rows(&self.machines);
        let at = screen.selected(&rows);
        let last = rows.len().saturating_sub(1);
        let go = |to: usize| rows.get(to).cloned();
        match input {
            Input::Close => self.account_screen = None,
            Input::Up => screen.chosen = at.and_then(|at| go(at.saturating_sub(1))),
            Input::Down => screen.chosen = at.and_then(|at| go((at + 1).min(last))),
            Input::Top => screen.chosen = go(0),
            Input::Bottom => screen.chosen = go(last),
            Input::Add => {
                let Some(host_id) = at.map(|at| rows[at].host_id().clone()) else {
                    return Vec::new();
                };
                match terminal::refusal(&self.machines, &host_id) {
                    Some(refusal) => self.notice = Some(format!("adding accounts: {refusal}")),
                    None => {
                        screen.adding = Some(AddAccount::for_provider(host_id, 0, &self.machines))
                    }
                }
            }
            Input::LogInAgain => {
                let Some(Pick::Account(host_id, account_id)) = at.map(|at| rows[at].clone()) else {
                    self.notice = Some("select an account to log in again".to_owned());
                    return Vec::new();
                };
                match terminal::refusal(&self.machines, &host_id) {
                    Some(refusal) => self.notice = Some(format!("logging in again: {refusal}")),
                    None => {
                        return vec![Effect::AttachTerminal {
                            host_id,
                            target: Target::LogInAgain(account_id),
                        }];
                    }
                }
            }
            Input::Dialog(_) => {}
        }
        Vec::new()
    }

    /// Whether the accounts screen is open on an account, which can be logged in again.
    pub(crate) fn on_account(&self) -> bool {
        self.account_screen.as_ref().is_some_and(|screen| {
            let rows = rows(&self.machines);
            screen
                .selected(&rows)
                .is_some_and(|at| matches!(rows[at], Pick::Account(..)))
        })
    }

    /// Pasted text for the add-account dialog of the accounts screen; returns whether it took
    /// the text.
    pub(crate) fn paste_accounts(&mut self, text: &str) -> bool {
        let Some(screen) = &mut self.account_screen else {
            return false;
        };
        if let Some(field) = screen.adding.as_mut().and_then(AddAccount::field) {
            field.push_str(text.trim().lines().next().unwrap_or(""));
        }
        true
    }
}

/// The account `account_id` of `host_id`'s machine, if listed.
pub fn find<'a>(
    machines: &'a [Machine],
    host_id: &HostId,
    account_id: &AccountId,
) -> Option<&'a Account> {
    machines
        .iter()
        .find(|machine| machine.host_id == *host_id)?
        .accounts
        .iter()
        .find(|account| account.account_id == *account_id)
}

/// A usage window's name as people say it: `five_hour` is the session limit, `seven_day` the
/// weekly one, and `seven_day_<model>` the weekly one of a model.
pub fn window_label(window: &str) -> String {
    // Codex names a window of a limit other than its own `<limit>.<length>`.
    if let Some((limit, length)) = window.split_once('.') {
        return format!("{} · {limit}", window_label(length));
    }
    match window {
        "five_hour" => "Session".to_owned(),
        "seven_day" | "weekly" => "Weekly".to_owned(),
        "daily" => "Daily".to_owned(),
        _ => match window.strip_prefix("seven_day_") {
            Some(model) => format!("Weekly · {}", capitalized(model)),
            None => window.replace('_', " "),
        },
    }
}

fn capitalized(word: &str) -> String {
    let mut chars = word.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// How long until `secs` seconds from now, as `2h 13m` or `5d 3h`.
pub fn until(secs: i64) -> String {
    let minutes = secs.max(0) / 60;
    let (days, hours, minutes) = (minutes / 1440, minutes / 60 % 24, minutes % 60);
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{minutes}m")
    }
}

#[cfg(test)]
mod tests {
    use herder_protocol::Provider;
    use ratatui::crossterm::event::KeyModifiers;

    use super::*;
    use crate::app::Msg;
    use crate::fake::{self, machine};

    fn press(app: &mut App, code: KeyCode) -> Vec<Effect> {
        app.update(Msg::Key(KeyEvent::new(code, KeyModifiers::NONE)))
    }

    fn app() -> App {
        let mut app = App::default();
        let mut h1 = machine("h1", "box", &[]);
        h1.accounts = vec![
            fake::account("claude-main", "Main"),
            fake::account("claude-work", "Work"),
        ];
        app.update(Msg::Machines(vec![h1, machine("h2", "laptop", &[])]));
        app
    }

    fn chosen(app: &App) -> Option<Pick> {
        let screen = app.account_screen.as_ref().unwrap();
        let rows = rows(&app.machines);
        screen.selected(&rows).map(|at| rows[at].clone())
    }

    #[test]
    fn the_screen_moves_over_machines_and_accounts_and_closes() {
        let mut app = app();
        press(&mut app, KeyCode::Char('A'));
        let main = Pick::Account(HostId::new("h1"), AccountId::new("claude-main"));
        assert_eq!(chosen(&app), Some(main));
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(chosen(&app), Some(Pick::Machine(HostId::new("h2"))));
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(chosen(&app), Some(Pick::Machine(HostId::new("h2"))));
        press(&mut app, KeyCode::Char('g'));
        assert_eq!(chosen(&app), Some(Pick::Machine(HostId::new("h1"))));
        // Keys meant for the session list do not reach it.
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.open, None);
        assert_eq!(press(&mut app, KeyCode::Char('r')), [Effect::Wake]);
        press(&mut app, KeyCode::Backspace);
        assert_eq!(app.account_screen, None);
    }

    #[test]
    fn n_runs_the_login_of_an_account_on_the_selected_machine() {
        let mut app = app();
        press(&mut app, KeyCode::Char('A'));
        press(&mut app, KeyCode::Char('G'));
        press(&mut app, KeyCode::Char('n'));
        let adding = |app: &App| app.account_screen.as_ref().unwrap().adding.clone();
        assert_eq!(adding(&app).unwrap().host_id, HostId::new("h2"));
        // Esc closes only the dialog.
        press(&mut app, KeyCode::Esc);
        assert_eq!(adding(&app), None);
        assert!(app.account_screen.is_some());

        press(&mut app, KeyCode::Char('n'));
        press(&mut app, KeyCode::Right);
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            [Effect::AttachTerminal {
                host_id: HostId::new("h2"),
                target: Target::Login(herder_client_core::NewAccount {
                    account_id: AccountId::new("codex"),
                    provider: Provider::Codex,
                    label: None,
                    config_dir: None,
                }),
            }]
        );
        // The screen stays, to show the account once its login succeeds.
        assert_eq!(adding(&app), None);
        assert!(app.account_screen.is_some());
    }

    #[test]
    fn l_logs_the_selected_account_in_again() {
        let mut app = app();
        press(&mut app, KeyCode::Char('A'));
        press(&mut app, KeyCode::Char('j'));
        assert!(app.on_account());
        assert_eq!(
            press(&mut app, KeyCode::Char('l')),
            [Effect::AttachTerminal {
                host_id: HostId::new("h1"),
                target: Target::LogInAgain(AccountId::new("claude-work")),
            }]
        );
        assert!(app.account_screen.is_some());
        // A machine's row has no account to log in.
        press(&mut app, KeyCode::Char('g'));
        assert!(!app.on_account());
        assert_eq!(press(&mut app, KeyCode::Char('l')), []);
        assert_eq!(
            app.notice.as_deref(),
            Some("select an account to log in again")
        );
    }

    #[test]
    fn members_cannot_log_accounts_in_again() {
        let mut app = App::default();
        let mut member = machine("h1", "box", &[]);
        member.role = Some(herder_protocol::Role::Member);
        member.accounts = vec![fake::account("claude-main", "Main")];
        app.update(Msg::Machines(vec![member]));
        press(&mut app, KeyCode::Char('A'));
        assert_eq!(press(&mut app, KeyCode::Char('l')), []);
        assert_eq!(
            app.notice.as_deref(),
            Some("logging in again: terminals are owner-only")
        );
    }

    #[test]
    fn members_cannot_add_accounts_from_the_screen() {
        let mut app = App::default();
        let mut member = machine("h1", "box", &[]);
        member.role = Some(herder_protocol::Role::Member);
        app.update(Msg::Machines(vec![member]));
        press(&mut app, KeyCode::Char('A'));
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(app.account_screen.as_ref().unwrap().adding, None);
        assert_eq!(
            app.notice.as_deref(),
            Some("adding accounts: terminals are owner-only")
        );
    }

    #[test]
    fn windows_are_named_as_people_say_them() {
        assert_eq!(window_label("five_hour"), "Session");
        assert_eq!(window_label("seven_day"), "Weekly");
        assert_eq!(window_label("weekly"), "Weekly");
        assert_eq!(window_label("seven_day_fable"), "Weekly · Fable");
        assert_eq!(window_label("gpt-5.weekly"), "Weekly · gpt-5");
        assert_eq!(window_label("90_minute"), "90 minute");
    }

    #[test]
    fn reset_times_count_down() {
        assert_eq!(until(-5), "0m");
        assert_eq!(until(59 * 60 + 59), "59m");
        assert_eq!(until(2 * 3600 + 13 * 60 + 30), "2h 13m");
        assert_eq!(until(5 * 86400 + 3 * 3600 + 59 * 60), "5d 3h");
    }
}
