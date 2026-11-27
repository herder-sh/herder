//! The hub's usage screen, from the fleet: what each of a hub's logins was used for over a
//! period on every machine, next to what is left of its limits, for the hub's owners. Its
//! state, keys and reducer.
//!
//! `u` on a hub in the fleet opens it: `get_hub_usage` and `list_provider_logins` to the hub.
//! There `j`/`k` move between logins, `enter` opens one to its machines, models and days, `p`
//! goes to the next period (24 hours, 7 days, 30 days) and `r` asks again.

use std::collections::{BTreeMap, BTreeSet};

use herder_client_core::ConnectionState;
use herder_protocol::{
    CommandBody, CommandResult, HostId, HubFailoverTotal, HubUsageTotal, Provider, ProviderLogin,
    Role, UsagePeriod, UsageTotal,
};
use ratatui::crossterm::event::{KeyCode, KeyEvent};

use crate::action::Action;
use crate::app::{App, Effect};
use crate::backup::Known;
use crate::compose::Origin;
use crate::secrets::Load;

/// The periods the screen goes through, in order.
pub const PERIODS: [UsagePeriod; 3] =
    [UsagePeriod::Day, UsagePeriod::Week, UsagePeriod::ThirtyDays];

/// The usage screen, over the fleet.
#[derive(Clone, Debug, PartialEq)]
pub struct HubUsage {
    /// The hub.
    pub hub: HostId,
    /// The period shown.
    pub period: UsagePeriod,
    /// What the hub added up over it.
    pub usage: Load<(Vec<HubUsageTotal>, Vec<HubFailoverTotal>)>,
    /// The hub's logins, with their limit windows.
    pub logins: Load<Vec<ProviderLogin>>,
    /// The login the user selected, by pin; until they move, the first one.
    pub chosen: Option<String>,
    /// The logins opened to their machines, models and days, by pin.
    pub open: BTreeSet<String>,
}

/// A command of the usage screen to a hub.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Sent {
    /// `get_hub_usage` over a period.
    Usage(HostId, UsagePeriod),
    /// `list_provider_logins`.
    Logins(HostId),
}

/// Input to the usage screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input {
    /// Close the screen.
    Close,
    /// Select the previous login.
    Up,
    /// Select the next login.
    Down,
    /// Open the selected login, or close it.
    Toggle,
    /// Go to the next period.
    Period,
    /// Ask the hub again.
    Reload,
}

/// The action a key asks for on the usage screen.
pub fn for_key(key: KeyEvent) -> Option<Action> {
    let input = match key.code {
        KeyCode::Esc | KeyCode::Backspace | KeyCode::Char('q') => Input::Close,
        KeyCode::Char('k') | KeyCode::Up => Input::Up,
        KeyCode::Char('j') | KeyCode::Down => Input::Down,
        KeyCode::Enter | KeyCode::Char(' ') => Input::Toggle,
        KeyCode::Char('p') => Input::Period,
        KeyCode::Char('r') => Input::Reload,
        KeyCode::Char('?') => return Some(Action::ToggleHelp),
        _ => return None,
    };
    Some(Action::HubUsage(input))
}

/// Tokens and dollars added up.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Amount {
    pub turns: u64,
    pub tokens: u64,
    pub cost_usd: f64,
    /// Some of `cost_usd` is herder's estimate, or a turn's cost was unknown.
    pub estimated: bool,
}

impl Amount {
    fn add(&mut self, total: &UsageTotal) {
        self.turns += total.turns;
        self.tokens += total.input + total.output + total.cache_read + total.cache_write;
        self.cost_usd += total.cost_usd;
        self.estimated |= total.cost_estimated;
    }

    /// "12.3M", compactly.
    pub fn tokens(&self) -> String {
        // Display only, to one decimal.
        let value = self.tokens as f64;
        match self.tokens {
            ..1_000 => self.tokens.to_string(),
            1_000..1_000_000 => format!("{:.1}K", value / 1e3),
            1_000_000..1_000_000_000 => format!("{:.1}M", value / 1e6),
            _ => format!("{:.1}B", value / 1e9),
        }
    }

    /// "$12.30", "<$0.01", or "~$12.30" when estimated: `~` reads the same in ASCII.
    pub fn dollars(&self) -> String {
        let value = if self.cost_usd > 0.0 && self.cost_usd < 0.01 {
            "<$0.01".to_owned()
        } else {
            format!("${:.2}", self.cost_usd)
        };
        if self.estimated {
            format!("~{value}")
        } else {
            value
        }
    }
}

/// A machine, model or day of a login, with what it used there.
#[derive(Clone, Debug, PartialEq)]
pub struct Part {
    pub name: String,
    pub amount: Amount,
}

/// A login's row: what it used over the period on every machine.
#[derive(Clone, Debug, PartialEq)]
pub struct LoginRow {
    pub pin: String,
    pub provider: Provider,
    /// The owner's name for it, else its email; its pin once the hub no longer holds it.
    pub title: String,
    /// The login as the hub lists it now; `None` for one it no longer holds.
    pub login: Option<ProviderLogin>,
    pub amount: Amount,
    pub limit_hits: u64,
    pub failovers_out: u64,
    pub failovers_in: u64,
    /// Most expensive first.
    pub machines: Vec<Part>,
    pub models: Vec<Part>,
    /// Newest first.
    pub days: Vec<Part>,
}

impl LoginRow {
    /// "3 limit hits · 2 failovers off it": what stood out; `None` when nothing did.
    pub fn limits(&self) -> Option<String> {
        let plural =
            |n: u64, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
        let mut parts = Vec::new();
        if self.limit_hits > 0 {
            parts.push(plural(self.limit_hits, "limit hit", "limit hits"));
        }
        if self.failovers_out > 0 {
            parts.push(plural(self.failovers_out, "failover", "failovers") + " off it");
        }
        if self.failovers_in > 0 {
            parts.push(format!("{} onto it", self.failovers_in));
        }
        (!parts.is_empty()).then(|| parts.join(" · "))
    }
}

/// Every login the hub holds, and any other with usage in the period, added up; most expensive
/// first, then by title. `host_name` names a host by its id.
pub fn report(
    totals: &[HubUsageTotal],
    failovers: &[HubFailoverTotal],
    logins: &[ProviderLogin],
    host_name: impl Fn(&HostId) -> String,
) -> Vec<LoginRow> {
    type Parts = BTreeMap<String, BTreeMap<String, Part>>;
    let mut rows: BTreeMap<String, LoginRow> = BTreeMap::new();
    let new =
        |pin: &str, provider: Provider, title: String, login: Option<ProviderLogin>| LoginRow {
            pin: pin.to_owned(),
            provider,
            title,
            login,
            amount: Amount::default(),
            limit_hits: 0,
            failovers_out: 0,
            failovers_in: 0,
            machines: Vec::new(),
            models: Vec::new(),
            days: Vec::new(),
        };
    for login in logins {
        let title = login.label.clone().unwrap_or_else(|| login.name.clone());
        rows.insert(
            login.pin.clone(),
            new(
                &login.pin,
                login.provider.clone(),
                title,
                Some(login.clone()),
            ),
        );
    }
    let (mut machines, mut models, mut days) = (Parts::new(), Parts::new(), Parts::new());
    let add = |parts: &mut Parts, pin: &str, key: &str, name: String, usage: &UsageTotal| {
        parts
            .entry(pin.to_owned())
            .or_default()
            .entry(key.to_owned())
            .or_insert_with(|| Part {
                name,
                amount: Amount::default(),
            })
            .amount
            .add(usage);
    };
    for total in totals {
        let pin = total.pin.as_str();
        let usage = &total.usage;
        rows.entry(pin.to_owned())
            .or_insert_with(|| new(pin, usage.provider.clone(), pin.to_owned(), None))
            .amount
            .add(usage);
        add(
            &mut machines,
            pin,
            total.host_id.as_str(),
            host_name(&total.host_id),
            usage,
        );
        let model = if usage.model.is_empty() {
            "default"
        } else {
            &usage.model
        };
        add(&mut models, pin, model, model.to_owned(), usage);
        add(&mut days, pin, &total.day, day(&total.day), usage);
    }
    for failover in failovers {
        if let Some(row) = rows.get_mut(&failover.pin) {
            row.limit_hits += failover.failovers.limit_hits;
            row.failovers_out += failover.failovers.failovers_out;
            row.failovers_in += failover.failovers.failovers_in;
        }
    }
    let costliest = |parts: Option<BTreeMap<String, Part>>| {
        let mut parts: Vec<Part> = parts.unwrap_or_default().into_values().collect();
        parts.sort_by(|a, b| {
            b.amount
                .cost_usd
                .total_cmp(&a.amount.cost_usd)
                .then(b.amount.tokens.cmp(&a.amount.tokens))
                .then(a.name.cmp(&b.name))
        });
        parts
    };
    let mut rows: Vec<LoginRow> = rows
        .into_values()
        .map(|mut row| {
            row.machines = costliest(machines.remove(&row.pin));
            row.models = costliest(models.remove(&row.pin));
            // Keyed by the day, `YYYY-MM-DD`, so newest first is the keys reversed.
            row.days = days
                .remove(&row.pin)
                .unwrap_or_default()
                .into_values()
                .rev()
                .collect();
            row
        })
        .collect();
    rows.sort_by(|a, b| {
        b.amount
            .cost_usd
            .total_cmp(&a.amount.cost_usd)
            .then(b.amount.tokens.cmp(&a.amount.tokens))
            .then(a.title.cmp(&b.title))
    });
    rows
}

/// A UTC day, `2026-11-26`, as "Thu 26 Nov".
pub fn day(day: &str) -> String {
    day.parse::<jiff::civil::Date>().map_or_else(
        |_| day.to_owned(),
        |date| date.strftime("%a %-d %b").to_string(),
    )
}

/// The period after `period`, round.
fn next(period: UsagePeriod) -> UsagePeriod {
    let at = PERIODS.iter().position(|p| *p == period).unwrap_or(0);
    PERIODS[(at + 1) % PERIODS.len()]
}

/// How the screen names a period: "last 7 days".
pub fn title(period: UsagePeriod) -> &'static str {
    match period {
        UsagePeriod::Day => "last 24 hours",
        UsagePeriod::Week => "last 7 days",
        UsagePeriod::ThirtyDays => "last 30 days",
        UsagePeriod::Month => "month to date",
    }
}

impl HubUsage {
    fn new(hub: HostId) -> Self {
        Self {
            hub,
            period: UsagePeriod::Week,
            usage: Load::Asking,
            logins: Load::Asking,
            chosen: None,
            open: BTreeSet::new(),
        }
    }

    /// The rows as the screen shows them, once both answers are in.
    pub fn rows(&self, host_name: impl Fn(&HostId) -> String) -> Option<Vec<LoginRow>> {
        match (&self.usage, &self.logins) {
            (Load::Loaded((totals, failovers)), Load::Loaded(logins)) => {
                Some(report(totals, failovers, logins, host_name))
            }
            _ => None,
        }
    }

    /// The selected row's index among `rows`.
    pub fn selected(&self, rows: &[LoginRow]) -> Option<usize> {
        if rows.is_empty() {
            return None;
        }
        Some(
            self.chosen
                .as_ref()
                .and_then(|pin| rows.iter().position(|row| row.pin == *pin))
                .unwrap_or(0),
        )
    }
}

/// `get_hub_usage` over `period` to `hub`.
fn ask_usage(hub: &HostId, period: UsagePeriod) -> Effect {
    Effect::Send {
        host_id: hub.clone(),
        command: CommandBody::GetHubUsage { period },
        origin: Origin::HubUsage(Sent::Usage(hub.clone(), period)),
    }
}

fn reload(hub: &HostId, period: UsagePeriod) -> Vec<Effect> {
    vec![
        ask_usage(hub, period),
        Effect::Send {
            host_id: hub.clone(),
            command: CommandBody::ListProviderLogins,
            origin: Origin::HubUsage(Sent::Logins(hub.clone())),
        },
    ]
}

impl App {
    /// Opens the usage screen of the fleet's selected machine, if it is a hub this client
    /// owns and reaches; else says why not.
    pub(crate) fn open_hub_usage(&mut self) -> Vec<Effect> {
        let Some(panel) = &self.machine_panel else {
            return Vec::new();
        };
        let Some(at) = panel.selected(&self.machines) else {
            return Vec::new();
        };
        let machine = &self.machines[at];
        let refusal = if machine.role.is_some_and(|role| role != Role::Owner) {
            Some("usage: only the hub's owners see what its logins used".to_owned())
        } else if machine.connection != ConnectionState::Connected {
            Some("usage: the machine is not connected".to_owned())
        } else if panel.links.get(&machine.host_id) != Some(&Known::Hub) {
            Some(format!(
                "usage: {} is not a hub; what hub logins used adds up on the hub, so select \
                 it and press u",
                machine.name
            ))
        } else {
            None
        };
        if let Some(refusal) = refusal {
            self.notice = Some(refusal);
            return Vec::new();
        }
        let screen = HubUsage::new(machine.host_id.clone());
        let effects = reload(&screen.hub, screen.period);
        if let Some(panel) = &mut self.machine_panel {
            panel.usage = Some(screen);
        }
        effects
    }

    /// Carries out one input to the usage screen.
    pub(crate) fn hub_usage_input(&mut self, input: Input) -> Vec<Effect> {
        let hub = self
            .machines
            .iter()
            .find(|m| Some(&m.host_id) == self.usage_screen().map(|s| &s.hub))
            .cloned();
        let host_name = |id: &HostId| host_name(hub.as_ref(), id);
        let Some(panel) = &mut self.machine_panel else {
            return Vec::new();
        };
        let Some(screen) = &mut panel.usage else {
            return Vec::new();
        };
        let rows = screen.rows(host_name).unwrap_or_default();
        let at = screen.selected(&rows);
        let pin = |to: usize| rows.get(to).map(|row| row.pin.clone());
        match input {
            Input::Close => panel.usage = None,
            Input::Up => screen.chosen = at.and_then(|at| pin(at.saturating_sub(1))),
            Input::Down => {
                screen.chosen = at.and_then(|at| pin((at + 1).min(rows.len().saturating_sub(1))));
            }
            Input::Toggle => {
                if let Some(pin) = at.and_then(pin)
                    && !screen.open.remove(&pin)
                {
                    screen.open.insert(pin);
                }
            }
            Input::Period => {
                screen.period = next(screen.period);
                screen.usage = Load::Asking;
                return vec![ask_usage(&screen.hub, screen.period)];
            }
            Input::Reload => {
                screen.usage = Load::Asking;
                screen.logins = Load::Asking;
                return reload(&screen.hub, screen.period);
            }
        }
        Vec::new()
    }

    /// Takes the hub's answer to a command of the usage screen.
    pub(crate) fn hub_usage_sent(&mut self, sent: Sent, result: Result<CommandResult, String>) {
        let Some(screen) = self
            .machine_panel
            .as_mut()
            .and_then(|panel| panel.usage.as_mut())
        else {
            return;
        };
        match (sent, result) {
            // An answer for a period no longer shown is dropped.
            (Sent::Usage(hub, period), result) if hub == screen.hub && period == screen.period => {
                screen.usage = match result {
                    Ok(CommandResult::HubUsage {
                        totals, failovers, ..
                    }) => Load::Loaded((totals, failovers)),
                    Ok(other) => Load::Failed(format!("unexpected answer {other:?}")),
                    Err(why) => Load::Failed(why),
                };
            }
            (Sent::Logins(hub), result) if hub == screen.hub => {
                screen.logins = match result {
                    Ok(CommandResult::ProviderLogins { logins }) => Load::Loaded(logins),
                    Ok(other) => Load::Failed(format!("unexpected answer {other:?}")),
                    Err(why) => Load::Failed(why),
                };
            }
            _ => {}
        }
    }

    fn usage_screen(&self) -> Option<&HubUsage> {
        self.machine_panel.as_ref()?.usage.as_ref()
    }
}

/// A host by the name the hub lists it under, else its id.
pub fn host_name(hub: Option<&herder_client_core::Machine>, host_id: &HostId) -> String {
    hub.and_then(|hub| hub.hosts.iter().find(|h| h.host_id == *host_id))
        .map_or_else(|| host_id.to_string(), |h| h.host_name.clone())
}

#[cfg(test)]
mod tests {
    use herder_protocol::{AccountId, FailoverTotal};

    use super::*;

    fn total(
        pin: &str,
        host: &str,
        day: &str,
        model: &str,
        turns: u64,
        cost: f64,
    ) -> HubUsageTotal {
        HubUsageTotal {
            pin: pin.into(),
            host_id: HostId::new(host),
            day: day.into(),
            usage: UsageTotal {
                account_id: AccountId::new(format!("vault-{pin}")),
                provider: Provider::Claude,
                model: model.into(),
                turns,
                input: turns * 100,
                output: turns * 10,
                cache_read: 0,
                cache_write: 0,
                cost_usd: cost,
                cost_estimated: false,
            },
        }
    }

    fn login(pin: &str, label: Option<&str>) -> ProviderLogin {
        ProviderLogin {
            name: format!("{pin}.json"),
            provider: Provider::Claude,
            label: label.map(str::to_owned),
            email: None,
            fallback: false,
            usage: Vec::new(),
            pin: pin.into(),
            state: herder_protocol::ProviderLoginState::Active,
            until: None,
            message: None,
        }
    }

    #[test]
    fn each_login_adds_up_over_its_machines_models_and_days() {
        let totals = [
            total("work", "h1", "2026-11-25", "opus", 4, 2.0),
            total("work", "h1", "2026-11-26", "opus", 1, 0.5),
            total("work", "h2", "2026-11-26", "sonnet", 3, 0.25),
            total("gone", "h1", "2026-11-20", "opus", 1, 0.125),
        ];
        let failovers = [HubFailoverTotal {
            pin: "work".into(),
            host_id: HostId::new("h1"),
            failovers: FailoverTotal {
                account_id: AccountId::new("vault-work"),
                limit_hits: 1,
                failovers_out: 1,
                failovers_in: 0,
            },
        }];
        let logins = [login("work", Some("Work")), login("idle", None)];
        let rows = report(&totals, &failovers, &logins, |id| format!("{id}-name"));
        let pins: Vec<&str> = rows.iter().map(|row| row.pin.as_str()).collect();
        assert_eq!(pins, ["work", "gone", "idle"]);
        let work = &rows[0];
        assert_eq!(work.title, "Work");
        assert_eq!((work.amount.turns, work.amount.tokens), (8, 880));
        assert_eq!(work.amount.dollars(), "$2.75");
        let machines: Vec<(&str, u64)> = work
            .machines
            .iter()
            .map(|p| (p.name.as_str(), p.amount.turns))
            .collect();
        assert_eq!(machines, [("h1-name", 5), ("h2-name", 3)]);
        let days: Vec<(&str, u64)> = work
            .days
            .iter()
            .map(|p| (p.name.as_str(), p.amount.turns))
            .collect();
        assert_eq!(days, [("Thu 26 Nov", 4), ("Wed 25 Nov", 4)]);
        assert_eq!(work.models[0].name, "opus");
        assert_eq!(
            work.limits().as_deref(),
            Some("1 limit hit · 1 failover off it")
        );
        // A login the hub no longer holds goes by its pin; one that used nothing is listed.
        assert!(rows[1].login.is_none() && rows[1].title == "gone");
        assert_eq!(
            (rows[2].title.as_str(), rows[2].amount.turns),
            ("idle.json", 0)
        );
        assert_eq!(rows[2].limits(), None);
    }

    #[test]
    fn amounts_read_compactly() {
        let amount = |tokens, cost_usd, estimated| Amount {
            turns: 1,
            tokens,
            cost_usd,
            estimated,
        };
        assert_eq!(amount(950, 0.0, false).tokens(), "950");
        assert_eq!(amount(12_345, 0.0, false).tokens(), "12.3K");
        assert_eq!(amount(4_500_000, 0.0, false).tokens(), "4.5M");
        assert_eq!(amount(1, 0.004, false).dollars(), "<$0.01");
        assert_eq!(amount(1, 3.4, true).dollars(), "~$3.40");
    }

    #[test]
    fn the_period_goes_round() {
        assert_eq!(next(UsagePeriod::Day), UsagePeriod::Week);
        assert_eq!(next(UsagePeriod::Week), UsagePeriod::ThirtyDays);
        assert_eq!(next(UsagePeriod::ThirtyDays), UsagePeriod::Day);
    }
}
