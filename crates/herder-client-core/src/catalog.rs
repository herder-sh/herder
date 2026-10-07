//! Shared names and models for the providers herder can run, and helpers for the quiet
//! “used on another machine” / “newer version elsewhere” hints.

use std::collections::HashSet;

use herder_protocol::{Account, AccountId, HostId, Provider, ProviderStatus};

use crate::Machine;

/// A model the menus offer, by the name the CLI takes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogModel {
    /// CLI id; empty is the provider's own default.
    pub id: String,
    /// Name as people write it.
    pub name: String,
    /// A few words on when to pick it.
    pub detail: Option<String>,
}

/// A provider's name as people write it.
pub fn provider_name(provider: &Provider) -> String {
    match provider {
        Provider::Claude => "Claude".into(),
        Provider::Codex => "Codex".into(),
        Provider::Cursor => "Cursor".into(),
        Provider::Grok => "Grok".into(),
        Provider::Opencode => "OpenCode".into(),
        Provider::Gemini => "Gemini".into(),
        Provider::Other(name) => {
            let mut chars = name.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().chain(chars).collect()
            })
        }
    }
}

/// The model new sessions of a provider start on; empty is the provider's own default.
pub fn default_model(provider: &Provider) -> String {
    match provider {
        Provider::Claude => "claude-opus-5-5".into(),
        _ => String::new(),
    }
}

/// The models the menus offer for `provider`.
pub fn models(provider: &Provider) -> Vec<CatalogModel> {
    match provider {
        Provider::Claude => vec![
            model("claude-opus-5-5", "Claude Opus 5.5", Some("Most capable")),
            model(
                "claude-sonnet-5-5",
                "Claude Sonnet 5.5",
                Some("Fast, for everyday work"),
            ),
            model("claude-fable-5-1", "Claude Fable 5.1", None),
            model(
                "claude-haiku-4-5-20251001",
                "Claude Haiku 4.5",
                Some("Fastest"),
            ),
        ],
        Provider::Codex => vec![
            model("gpt-6.1-sol", "GPT-6.1 Sol", Some("Most capable")),
            model("gpt-6-luna", "GPT-6 Luna", None),
        ],
        Provider::Grok => vec![
            model("grok-4.6", "Grok 4.6", Some("Most capable")),
            model("grok-4.5", "Grok 4.5", None),
        ],
        Provider::Cursor => vec![
            model("", "Cursor Auto", Some("Lets Cursor pick")),
            model("composer-2.5", "Composer 2.5", Some("Cursor's agent model")),
            model(
                "composer-2.5-fast",
                "Composer 2.5 Fast",
                Some("Faster Composer"),
            ),
        ],
        Provider::Opencode => vec![model("", "OpenCode default", None)],
        Provider::Gemini | Provider::Other(_) => Vec::new(),
    }
}

/// A model's name for the menu: the catalog's, else its id, else the provider's default.
pub fn model_name(id: &str, provider: &Provider) -> String {
    if id.is_empty() {
        return format!("{} default", provider_name(provider));
    }
    models(provider)
        .into_iter()
        .find(|model| model.id == id)
        .map(|model| model.name)
        .unwrap_or_else(|| id.to_owned())
}

/// Providers that have an account on another of `machines` but not on `host_id`.
pub fn used_elsewhere(machines: &[Machine], host_id: &HostId) -> Vec<Provider> {
    let here: HashSet<Provider> = machines
        .iter()
        .find(|machine| machine.host_id == *host_id)
        .map(|machine| {
            machine
                .accounts
                .iter()
                .map(|account| account.provider.clone())
                .collect()
        })
        .unwrap_or_default();
    let mut elsewhere = HashSet::new();
    for machine in machines {
        if machine.host_id == *host_id {
            continue;
        }
        for account in &machine.accounts {
            if !here.contains(&account.provider) {
                elsewhere.insert(account.provider.clone());
            }
        }
    }
    let mut missing: Vec<_> = elsewhere.into_iter().collect();
    missing.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    missing
}

/// Machine names that already have an account for `provider`, other than `host_id`.
pub fn used_on(machines: &[Machine], host_id: &HostId, provider: &Provider) -> Vec<String> {
    machines
        .iter()
        .filter(|machine| machine.host_id != *host_id)
        .filter(|machine| {
            machine
                .accounts
                .iter()
                .any(|account| account.provider == *provider)
        })
        .map(|machine| machine.name.clone())
        .collect()
}

/// Whether `newer` looks like a later `--version` than `older`.
pub fn version_newer(newer: &str, older: &str) -> bool {
    version_key(newer) > version_key(older)
}

/// A later version of `provider` reported on another machine, if this host has an older one.
pub fn newer_elsewhere<'a>(
    machines: &'a [Machine],
    host_id: &HostId,
    provider: &Provider,
) -> Option<&'a Machine> {
    let here =
        status_on(machines, host_id, provider).and_then(|status| status.version.as_deref())?;
    machines.iter().find(|machine| {
        if machine.host_id == *host_id {
            return false;
        }
        status_on(machines, &machine.host_id, provider)
            .and_then(|status| status.version.as_deref())
            .is_some_and(|version| version_newer(version, here))
    })
}

/// `provider`'s status on `host_id`, if that machine sent one.
pub fn status_on<'a>(
    machines: &'a [Machine],
    host_id: &HostId,
    provider: &Provider,
) -> Option<&'a ProviderStatus> {
    machines
        .iter()
        .find(|machine| machine.host_id == *host_id)?
        .providers
        .iter()
        .find(|status| status.provider == *provider)
}

/// An unused account id for `provider` on this machine: `cursor`, then `cursor-2`.
pub fn next_account_id(accounts: &[Account], provider: &Provider) -> String {
    let base = provider.as_str();
    if !taken(accounts, base) {
        return base.to_owned();
    }
    (2..)
        .find_map(|n| {
            let id = format!("{base}-{n}");
            (!taken(accounts, &id)).then_some(id)
        })
        .unwrap_or_else(|| format!("{base}-new"))
}

fn taken(accounts: &[Account], id: &str) -> bool {
    accounts
        .iter()
        .any(|account| account.account_id == AccountId::new(id))
}

fn model(id: &str, name: &str, detail: Option<&str>) -> CatalogModel {
    CatalogModel {
        id: id.to_owned(),
        name: name.to_owned(),
        detail: detail.map(str::to_owned),
    }
}

fn version_key(raw: &str) -> Vec<u64> {
    raw.split(|c: char| !c.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.parse().ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use herder_protocol::{FailoverSettings, Role};

    use super::*;
    use crate::{ConnectionQuality, ConnectionState, Machine};

    fn machine(
        host: &str,
        name: &str,
        providers: &[Provider],
        statuses: Vec<ProviderStatus>,
    ) -> Machine {
        Machine {
            host_id: HostId::new(host),
            name: name.to_owned(),
            addresses: Vec::new(),
            address: None,
            fingerprint: "ab".repeat(32),
            connection: ConnectionState::Connected,
            quality: ConnectionQuality::default(),
            role: Some(Role::Owner),
            sessions: Vec::new(),
            hosts: Vec::new(),
            projects: Vec::new(),
            accounts: providers
                .iter()
                .map(|provider| Account {
                    account_id: AccountId::new(provider.as_str()),
                    provider: provider.clone(),
                    label: provider.as_str().to_owned(),
                    config_dir: None,
                    usage: Vec::new(),
                })
                .collect(),
            failover: FailoverSettings::default(),
            terminals: Vec::new(),
            resources: None,
            session_usage: Default::default(),
            vault: None,
            skills: None,
            session_skills: Default::default(),
            providers: statuses,
        }
    }

    fn status(provider: Provider, version: &str) -> ProviderStatus {
        ProviderStatus {
            provider,
            installed: true,
            version: Some(version.to_owned()),
            binary: None,
            can_install: true,
            can_update: true,
        }
    }

    #[test]
    fn used_elsewhere_is_providers_on_other_machines_only() {
        let box_m = machine("box", "box", &[Provider::Claude], Vec::new());
        let laptop = machine(
            "laptop",
            "laptop",
            &[Provider::Claude, Provider::Cursor],
            Vec::new(),
        );
        assert_eq!(
            used_elsewhere(&[box_m.clone(), laptop.clone()], &HostId::new("box")),
            [Provider::Cursor]
        );
        assert!(used_elsewhere(&[box_m, laptop], &HostId::new("laptop")).is_empty());
    }

    #[test]
    fn newer_elsewhere_compares_version_numbers() {
        assert!(version_newer("2.1.0", "2.0.9"));
        assert!(!version_newer("2.0.9", "2.1.0"));
        let old = machine(
            "box",
            "box",
            &[],
            vec![status(Provider::Cursor, "agent 0.1.0")],
        );
        let new = machine(
            "laptop",
            "laptop",
            &[],
            vec![status(Provider::Cursor, "agent 0.2.0")],
        );
        assert_eq!(
            newer_elsewhere(
                &[old.clone(), new.clone()],
                &HostId::new("box"),
                &Provider::Cursor
            )
            .map(|m| m.name.as_str()),
            Some("laptop")
        );
        assert!(newer_elsewhere(&[old, new], &HostId::new("laptop"), &Provider::Cursor).is_none());
    }

    #[test]
    fn next_account_id_skips_ones_already_there() {
        let accounts = vec![Account {
            account_id: AccountId::new("cursor"),
            provider: Provider::Cursor,
            label: "cursor".into(),
            config_dir: None,
            usage: Vec::new(),
        }];
        assert_eq!(next_account_id(&accounts, &Provider::Cursor), "cursor-2");
        assert_eq!(next_account_id(&[], &Provider::Claude), "claude");
    }
}
