//! Provider accounts on this host: the adapter that runs each provider, and the account list
//! clients see.
//!
//! Accounts come from the daemon config ([`crate::config`]) and live only there: an account is
//! a pointer at a login the provider's own CLI keeps, so there is nothing for herder to store.
//! herder never reads, copies or relays what is in an account's config dir; the adapter only
//! sets the CLI's config dir variable.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use herder_adapters::Adapter;
use herder_adapters::acp::{AcpAdapter, AgentProfile};
use herder_adapters::claude::ClaudeAdapter;
use herder_adapters::codex::CodexAdapter;
use herder_protocol::{Account, Provider};

use crate::session::{Accounts, Adapters};
use crate::usage::{Probe, Probes, Windows};

/// Every provider herder can run sessions on.
pub const PROVIDERS: [Provider; 5] = [
    Provider::Claude,
    Provider::Codex,
    Provider::Cursor,
    Provider::Grok,
    Provider::Opencode,
];

/// Whether herder can run `provider`'s sessions.
pub fn runs(provider: &Provider) -> bool {
    PROVIDERS.contains(provider)
}

/// The adapter for every provider at least one account belongs to, each running the binary
/// `binaries` names for it, else the provider's own CLI on `PATH`.
pub fn adapters(accounts: &Accounts, binaries: &HashMap<Provider, PathBuf>) -> Adapters {
    let mut adapters = Adapters::new();
    for account in accounts.values() {
        let provider = &account.provider;
        if let Some(adapter) = adapter(provider, binaries.get(provider).cloned()) {
            adapters.register(provider.clone(), adapter);
        }
    }
    adapters
}

/// The adapter that runs `provider` on `binary`; `None` when herder cannot run it.
fn adapter(provider: &Provider, binary: Option<PathBuf>) -> Option<Arc<dyn Adapter>> {
    let acp = |mut profile: AgentProfile| {
        if let Some(binary) = &binary {
            profile.program = binary.to_string_lossy().into_owned();
        }
        Arc::new(AcpAdapter::new(profile)) as Arc<dyn Adapter>
    };
    Some(match provider {
        Provider::Claude => Arc::new(match binary {
            Some(program) => ClaudeAdapter { program },
            None => ClaudeAdapter::default(),
        }),
        Provider::Codex => Arc::new(match binary {
            Some(program) => CodexAdapter { program },
            None => CodexAdapter::default(),
        }),
        Provider::Cursor => acp(AgentProfile::cursor()),
        Provider::Grok => acp(AgentProfile::grok()),
        Provider::Opencode => acp(AgentProfile::opencode()),
        Provider::Gemini | Provider::Other(_) => return None,
    })
}

/// The usage probe ([`crate::usage`]) for every provider that has one and at least one account,
/// running the same binary as its adapter.
pub fn probes(accounts: &Accounts, binaries: &HashMap<Provider, PathBuf>) -> Probes {
    let mut probes = Probes::new();
    for account in accounts.values() {
        let provider = &account.provider;
        let binary = binaries.get(provider).cloned();
        let probe: Arc<dyn Probe> = match provider {
            Provider::Claude => Arc::new(match binary {
                Some(program) => ClaudeAdapter { program },
                None => ClaudeAdapter::default(),
            }),
            Provider::Codex => Arc::new(match binary {
                Some(program) => CodexAdapter { program },
                None => CodexAdapter::default(),
            }),
            _ => continue,
        };
        probes.insert(provider.clone(), probe);
    }
    probes
}

/// `accounts` as clients see them, ordered by id, each with the windows `usage` holds for it;
/// none until its provider reports one.
pub(crate) fn list(accounts: &Accounts, usage: &Windows) -> Vec<Account> {
    accounts
        .iter()
        .map(|(id, account)| Account {
            account_id: id.clone(),
            provider: account.provider.clone(),
            label: account.label.clone(),
            usage: usage.get(id).cloned().unwrap_or_default(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    use herder_adapters::StartRequest;
    use herder_protocol::{AccountId, PermissionMode, UsageWindow};

    use super::*;
    use crate::session::AccountConfig;

    fn account(provider: Provider, config_dir: Option<&Path>) -> AccountConfig {
        AccountConfig {
            provider,
            label: "Label".into(),
            config_dir: config_dir.map(Path::to_owned),
            failover: false,
        }
    }

    /// A stand-in CLI that writes its environment to `$OUT` and exits.
    fn env_dumper(dir: &Path) -> PathBuf {
        let path = dir.join("cli");
        std::fs::write(&path, "#!/bin/sh\nenv > \"$OUT\"\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// Starts `provider`'s registered adapter for `account` and returns the environment the
    /// CLI saw.
    async fn cli_env(adapters: &Adapters, account: &AccountConfig, dir: &Path) -> Vec<String> {
        let out = dir.join(format!("{}.env", account.provider.as_str()));
        let request = StartRequest {
            config_dir: account.config_dir.clone(),
            env: BTreeMap::from([
                ("OUT".to_owned(), out.to_str().unwrap().to_owned()),
                ("PATH".to_owned(), "/usr/bin:/bin".to_owned()),
            ]),
            cwd: dir.to_owned(),
            model: None,
            permission_mode: PermissionMode::Ask,
            seed: Vec::new(),
            mcp: None,
            launcher: Vec::new(),
        };
        let adapter = adapters.get(&account.provider).unwrap();
        // The stand-in exits at once, so the start fails; only its environment matters.
        assert!(adapter.start(request).await.is_err());
        let mut env: Vec<_> = std::fs::read_to_string(out)
            .unwrap()
            .lines()
            // Set by the shell itself.
            .filter(|line| {
                !["PWD=", "OLDPWD=", "SHLVL=", "_="]
                    .iter()
                    .any(|v| line.starts_with(v))
            })
            .map(str::to_owned)
            .collect();
        env.sort();
        env
    }

    #[tokio::test]
    async fn adapters_run_the_configured_binary_with_only_the_accounts_config_dir() {
        let dir = tempfile::tempdir().unwrap();
        let cli = env_dumper(dir.path());
        let codex_home = dir.path().join("codex-home");
        let claude = account(Provider::Claude, None);
        let codex = account(Provider::Codex, Some(&codex_home));
        let accounts = Accounts::from([
            (AccountId::new("claude"), claude.clone()),
            (AccountId::new("codex"), codex.clone()),
        ]);
        let binaries = HashMap::from([
            (Provider::Claude, cli.clone()),
            (Provider::Codex, cli.clone()),
        ]);
        let adapters = adapters(&accounts, &binaries);
        assert!(adapters.get(&Provider::Cursor).is_none());

        let out = |provider: &str| format!("OUT={}/{provider}.env", dir.path().display());
        assert_eq!(
            cli_env(&adapters, &claude, dir.path()).await,
            [out("claude"), "PATH=/usr/bin:/bin".into()]
        );
        assert_eq!(
            cli_env(&adapters, &codex, dir.path()).await,
            [
                format!("CODEX_HOME={}", codex_home.display()),
                out("codex"),
                "PATH=/usr/bin:/bin".into()
            ]
        );
    }

    #[test]
    fn adapters_cover_every_provider_with_an_account() {
        let accounts = Accounts::from(
            PROVIDERS.map(|provider| (AccountId::new(provider.as_str()), account(provider, None))),
        );
        let adapters = adapters(&accounts, &HashMap::new());
        for provider in PROVIDERS {
            assert!(adapters.get(&provider).is_some(), "{}", provider.as_str());
        }
        assert!(adapters.get(&Provider::Gemini).is_none());
        assert!(super::adapter(&Provider::Gemini, None).is_none());
    }

    #[test]
    fn list_shows_each_account_with_its_usage() {
        let accounts = Accounts::from([
            (
                AccountId::new("claude-main"),
                account(Provider::Claude, Some(Path::new("/secret/dir"))),
            ),
            (AccountId::new("codex"), account(Provider::Codex, None)),
        ]);
        let window = UsageWindow {
            window: "five_hour".into(),
            used_percent: 9.0,
            resets_at: None,
        };
        let usage = Windows::from([(AccountId::new("claude-main"), vec![window.clone()])]);
        assert_eq!(
            list(&accounts, &usage),
            [
                Account {
                    account_id: AccountId::new("claude-main"),
                    provider: Provider::Claude,
                    label: "Label".into(),
                    usage: vec![window],
                },
                Account {
                    account_id: AccountId::new("codex"),
                    provider: Provider::Codex,
                    label: "Label".into(),
                    usage: Vec::new(),
                }
            ]
        );
    }

    #[test]
    fn probes_cover_claude_and_codex_accounts_only() {
        let accounts = Accounts::from(
            PROVIDERS.map(|provider| (AccountId::new(provider.as_str()), account(provider, None))),
        );
        let probes = probes(&accounts, &HashMap::new());
        let mut providers: Vec<_> = probes.keys().map(Provider::as_str).collect();
        providers.sort_unstable();
        assert_eq!(providers, ["claude", "codex"]);
    }
}
