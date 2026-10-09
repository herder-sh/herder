//! The providers herder can add, and the models each one offers, so the TUI and the apps
//! stay on one list.

use herder_protocol::Provider;

use crate::Machine;

/// A named model in [`provider_catalog`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CatalogModel {
    /// The id the CLI takes.
    pub id: &'static str,
    /// The name shown in pickers.
    pub name: &'static str,
    /// A few words on when to pick it.
    pub detail: Option<&'static str>,
}

/// One provider in the shared catalog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogEntry {
    /// The provider.
    pub provider: Provider,
    /// How people write its name.
    pub display_name: &'static str,
    /// The model a new session starts on; empty is the provider's own default.
    pub default_model: &'static str,
    /// Named models the switch picker offers.
    pub models: &'static [CatalogModel],
}

const CLAUDE: &[CatalogModel] = &[
    CatalogModel {
        id: "claude-opus-5-5",
        name: "Claude Opus 5.5",
        detail: Some("Most capable"),
    },
    CatalogModel {
        id: "claude-sonnet-5-5",
        name: "Claude Sonnet 5.5",
        detail: Some("Fast, for everyday work"),
    },
    CatalogModel {
        id: "claude-fable-5-1",
        name: "Claude Fable 5.1",
        detail: None,
    },
    CatalogModel {
        id: "claude-haiku-4-5-20251001",
        name: "Claude Haiku 4.5",
        detail: Some("Fastest"),
    },
];

const CODEX: &[CatalogModel] = &[
    CatalogModel {
        id: "gpt-6.1-sol",
        name: "GPT-6.1 Sol",
        detail: Some("Most capable"),
    },
    CatalogModel {
        id: "gpt-6-luna",
        name: "GPT-6 Luna",
        detail: None,
    },
];

const CURSOR: &[CatalogModel] = &[
    CatalogModel {
        id: "auto",
        name: "Auto",
        detail: Some("Picks for the task"),
    },
    CatalogModel {
        id: "composer-2.5",
        name: "Composer 2.5",
        detail: Some("Cursor's agent"),
    },
    CatalogModel {
        id: "composer-2.5-fast",
        name: "Composer 2.5 Fast",
        detail: None,
    },
];

const GROK: &[CatalogModel] = &[
    CatalogModel {
        id: "grok-4.6",
        name: "Grok 4.6",
        detail: Some("Most capable"),
    },
    CatalogModel {
        id: "grok-4.5",
        name: "Grok 4.5",
        detail: None,
    },
];

const OPENCODE: &[CatalogModel] = &[
    CatalogModel {
        id: "opencode/grok-code",
        name: "Grok Code",
        detail: None,
    },
    CatalogModel {
        id: "opencode/claude",
        name: "Claude",
        detail: None,
    },
    CatalogModel {
        id: "opencode/gpt",
        name: "GPT",
        detail: None,
    },
];

const CATALOG: &[CatalogEntry] = &[
    CatalogEntry {
        provider: Provider::Claude,
        display_name: "Claude",
        default_model: "claude-opus-5-5",
        models: CLAUDE,
    },
    CatalogEntry {
        provider: Provider::Codex,
        display_name: "Codex",
        default_model: "",
        models: CODEX,
    },
    CatalogEntry {
        provider: Provider::Cursor,
        display_name: "Cursor",
        default_model: "auto",
        models: CURSOR,
    },
    CatalogEntry {
        provider: Provider::Opencode,
        display_name: "OpenCode",
        default_model: "",
        models: OPENCODE,
    },
    CatalogEntry {
        provider: Provider::Grok,
        display_name: "Grok",
        default_model: "",
        models: GROK,
    },
];

/// The providers the add-account dialog and model pickers share, in that order.
pub fn provider_catalog() -> &'static [CatalogEntry] {
    CATALOG
}

/// The catalog row for `provider`, if herder names one.
pub fn catalog_entry(provider: &Provider) -> Option<&'static CatalogEntry> {
    CATALOG.iter().find(|entry| entry.provider == *provider)
}

/// The next account id for `provider` that is not in `taken`: `cursor`, then `cursor-2`.
pub fn next_account_id<'a>(
    provider: &Provider,
    taken: impl IntoIterator<Item = &'a str>,
) -> String {
    let taken: Vec<&str> = taken.into_iter().collect();
    let stem = provider.as_str();
    if !taken.contains(&stem) {
        return stem.to_owned();
    }
    for n in 2.. {
        let id = format!("{stem}-{n}");
        if !taken.contains(&id.as_str()) {
            return id;
        }
    }
    stem.to_owned()
}

/// Why a machine should mention a provider after its accounts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderHintKind {
    /// Another of your machines has an account of this provider; this one does not.
    Missing {
        /// Those machines' names.
        on: Vec<String>,
    },
    /// This host's CLI is older than another of your machines', or it has a dedicated updater.
    Update {
        /// This host's version.
        version: String,
        /// The newer version seen elsewhere, when that is why.
        newer: Option<String>,
    },
}

/// One muted line for a gap on one machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderHint {
    /// The provider the line is about.
    pub provider: Provider,
    /// Why.
    pub kind: ProviderHintKind,
}

impl ProviderHint {
    /// The muted line, such as `also on laptop: cursor` or `cursor 0.1.0 · update`.
    pub fn text(&self) -> String {
        match &self.kind {
            ProviderHintKind::Missing { on } => {
                format!("also on {}: {}", on.join(", "), self.provider.as_str())
            }
            ProviderHintKind::Update { version, .. } => {
                format!("{} {version} · update", self.provider.as_str())
            }
        }
    }
}

/// The quiet lines for `machine`, inferred from `all` the machines you own.
pub fn provider_hints(machine: &Machine, all: &[Machine]) -> Vec<ProviderHint> {
    let mut hints = Vec::new();
    let here: Vec<&Provider> = machine
        .accounts
        .iter()
        .map(|account| &account.provider)
        .collect();
    for entry in CATALOG {
        let elsewhere: Vec<String> = all
            .iter()
            .filter(|other| other.host_id != machine.host_id)
            .filter(|other| {
                other
                    .accounts
                    .iter()
                    .any(|account| account.provider == entry.provider)
            })
            .map(|other| other.name.clone())
            .collect();
        if !elsewhere.is_empty() && !here.iter().any(|have| **have == entry.provider) {
            hints.push(ProviderHint {
                provider: entry.provider.clone(),
                kind: ProviderHintKind::Missing { on: elsewhere },
            });
            continue;
        }
        let Some(status) = machine
            .providers
            .iter()
            .find(|status| status.provider == entry.provider)
        else {
            continue;
        };
        if !status.installed {
            continue;
        }
        let newer = all
            .iter()
            .filter(|other| other.host_id != machine.host_id)
            .flat_map(|other| &other.providers)
            .filter(|other| other.provider == entry.provider)
            .filter_map(|other| other.version.as_deref())
            .find(|other| version_newer(other, status.version.as_deref().unwrap_or("")));
        if let Some(newer) = newer {
            hints.push(ProviderHint {
                provider: entry.provider.clone(),
                kind: ProviderHintKind::Update {
                    version: status.version.clone().unwrap_or_default(),
                    newer: Some(newer.to_owned()),
                },
            });
        }
    }
    hints
}

/// Whether `left` looks newer than `right`, by leading dotted numbers.
fn version_newer(left: &str, right: &str) -> bool {
    compare_versions(left) > compare_versions(right)
}

fn compare_versions(text: &str) -> Vec<u32> {
    let digits = text
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect::<String>();
    digits
        .split('.')
        .filter_map(|part| part.parse().ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use herder_protocol::{Account, AccountId, FailoverSettings, HostId, Provider, ProviderStatus};

    use super::*;
    use crate::{ConnectionQuality, ConnectionState, Machine};

    fn machine(
        id: &str,
        name: &str,
        accounts: &[Provider],
        versions: &[(&Provider, &str)],
    ) -> Machine {
        Machine {
            host_id: HostId::new(id),
            name: name.into(),
            addresses: Vec::new(),
            address: None,
            fingerprint: String::new(),
            connection: ConnectionState::Connected,
            quality: ConnectionQuality::default(),
            role: None,
            sessions: Vec::new(),
            hosts: Vec::new(),
            projects: Vec::new(),
            accounts: accounts
                .iter()
                .map(|provider| Account {
                    account_id: AccountId::new(provider.as_str()),
                    provider: provider.clone(),
                    label: provider.as_str().into(),
                    config_dir: None,
                    email: None,
                    usage: Vec::new(),
                    fallback: false,
                })
                .collect(),
            failover: FailoverSettings::default(),
            providers: versions
                .iter()
                .map(|(provider, version)| ProviderStatus {
                    provider: (*provider).clone(),
                    installed: true,
                    version: Some((*version).into()),
                    binary: Some(provider.as_str().into()),
                    can_install: true,
                    can_update: true,
                })
                .collect(),
            terminals: Vec::new(),
            resources: None,
            session_usage: Default::default(),
            vault: None,
            skills: None,
            session_skills: Default::default(),
        }
    }

    #[test]
    fn used_elsewhere_is_the_other_machines_accounts() {
        let laptop = machine("a", "laptop", &[Provider::Cursor], &[]);
        let box_ = machine("b", "box", &[], &[]);
        let hints = provider_hints(&box_, &[laptop.clone(), box_.clone()]);
        assert_eq!(hints.len(), 1);
        assert_eq!(hints[0].provider, Provider::Cursor);
        assert_eq!(hints[0].text(), "also on laptop: cursor");
        assert!(provider_hints(&laptop, &[laptop.clone(), box_]).is_empty());
    }

    #[test]
    fn no_hint_when_every_machine_already_has_the_provider() {
        let a = machine("a", "laptop", &[Provider::Claude], &[]);
        let b = machine("b", "box", &[Provider::Claude], &[]);
        assert!(provider_hints(&a, &[a.clone(), b.clone()]).is_empty());
        assert!(provider_hints(&b, &[a, b.clone()]).is_empty());
    }

    #[test]
    fn an_older_version_wants_an_update() {
        let laptop = machine("a", "laptop", &[], &[(&Provider::Cursor, "0.2.0")]);
        let box_ = machine("b", "box", &[], &[(&Provider::Cursor, "0.1.0")]);
        let hints = provider_hints(&box_, &[laptop, box_.clone()]);
        let update = hints
            .iter()
            .find(|hint| hint.provider == Provider::Cursor)
            .unwrap();
        assert_eq!(update.text(), "cursor 0.1.0 · update");
        match &update.kind {
            ProviderHintKind::Update { newer, .. } => {
                assert_eq!(newer.as_deref(), Some("0.2.0"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn next_id_skips_ones_already_taken() {
        assert_eq!(next_account_id(&Provider::Cursor, []), "cursor");
        assert_eq!(
            next_account_id(&Provider::Cursor, ["cursor", "cursor-2"]),
            "cursor-3"
        );
    }

    #[test]
    fn cursor_has_named_models() {
        let entry = catalog_entry(&Provider::Cursor).unwrap();
        assert!(entry.models.len() > 1);
        assert!(entry.models.iter().any(|model| model.id == "composer-2.5"));
    }
}
