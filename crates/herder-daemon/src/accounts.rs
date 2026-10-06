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

use crate::session::{Accounts, Adapters, TitleCli, TitleClis};
use crate::usage::{Known, Probe, Probes};

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

/// The adapter for every provider herder runs, each running the binary `binaries` names for
/// it, else the provider's own CLI on `PATH`; ready for accounts added later.
pub fn adapters(binaries: &HashMap<Provider, PathBuf>) -> Adapters {
    let mut adapters = Adapters::new();
    for provider in PROVIDERS {
        if let Some(adapter) = adapter(&provider, binaries.get(&provider).cloned()) {
            adapters.register(provider, adapter);
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

/// The CLI herder runs `provider`'s sessions with: the binary `binaries` names for it, else the
/// provider's own CLI, looked up on `PATH`; `None` when herder cannot run it.
pub fn program(provider: &Provider, binaries: &HashMap<Provider, PathBuf>) -> Option<PathBuf> {
    if !runs(provider) {
        return None;
    }
    if let Some(binary) = binaries.get(provider) {
        return Some(binary.clone());
    }
    Some(match provider {
        Provider::Claude => ClaudeAdapter::default().program,
        Provider::Codex => CodexAdapter::default().program,
        Provider::Cursor => PathBuf::from(AgentProfile::cursor().program),
        Provider::Grok => PathBuf::from(AgentProfile::grok().program),
        Provider::Opencode => PathBuf::from(AgentProfile::opencode().program),
        Provider::Gemini | Provider::Other(_) => return None,
    })
}

/// The usage probe ([`crate::usage`]) for every provider that has one, running the same binary
/// as its adapter; ready for accounts added later.
pub fn probes(binaries: &HashMap<Provider, PathBuf>) -> Probes {
    let mut probes = Probes::new();
    for provider in &PROVIDERS {
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

/// The CLI each provider that can title sessions titles them with ([`crate::session::titles`]),
/// the same binary as its adapter.
pub fn title_clis(binaries: &HashMap<Provider, PathBuf>) -> TitleClis {
    let program = |provider: &Provider, name: &str| {
        binaries
            .get(provider)
            .cloned()
            .unwrap_or_else(|| PathBuf::from(name))
    };
    TitleClis::from([
        (
            Provider::Claude,
            TitleCli::claude(program(&Provider::Claude, "claude")),
        ),
        (
            Provider::Codex,
            TitleCli::codex(program(&Provider::Codex, "codex")),
        ),
    ])
}

/// `accounts` as clients see them, ordered by id, each with the email and windows `usage`
/// holds for it; none until its provider reports them.
pub(crate) fn list(accounts: &Accounts, usage: &Known) -> Vec<Account> {
    accounts
        .iter()
        .map(|(id, account)| Account {
            config_dir: account
                .config_dir
                .as_ref()
                .map(|dir| dir.to_string_lossy().into_owned()),
            account_id: id.clone(),
            provider: account.provider.clone(),
            label: account.label.clone(),
            email: usage.emails.get(id).cloned(),
            usage: usage.windows.get(id).cloned().unwrap_or_default(),
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
            resume: None,
            mcp: None,
            launcher: Vec::new(),
            skills: None,
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
        let binaries = HashMap::from([
            (Provider::Claude, cli.clone()),
            (Provider::Codex, cli.clone()),
        ]);
        let adapters = adapters(&binaries);

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
    fn adapters_cover_every_provider() {
        let adapters = adapters(&HashMap::new());
        for provider in PROVIDERS {
            assert!(adapters.get(&provider).is_some(), "{}", provider.as_str());
        }
        assert!(adapters.get(&Provider::Gemini).is_none());
        assert!(super::adapter(&Provider::Gemini, None).is_none());
    }

    #[test]
    fn program_is_the_configured_binary_else_the_providers_own_cli() {
        let binaries = HashMap::from([(Provider::Codex, PathBuf::from("/opt/codex"))]);
        let program = |provider: &Provider| program(provider, &binaries);
        assert_eq!(program(&Provider::Codex), Some(PathBuf::from("/opt/codex")));
        assert_eq!(program(&Provider::Claude), Some(PathBuf::from("claude")));
        assert_eq!(
            program(&Provider::Cursor),
            Some(PathBuf::from(AgentProfile::cursor().program))
        );
        for provider in PROVIDERS {
            assert!(program(&provider).is_some(), "{}", provider.as_str());
        }
        assert_eq!(program(&Provider::Gemini), None);
    }

    #[test]
    fn claude_and_codex_title_with_the_configured_binary_and_a_small_model() {
        let binaries = HashMap::from([(Provider::Codex, PathBuf::from("/opt/codex"))]);
        let clis = title_clis(&binaries);
        assert_eq!(clis.len(), 2);
        let claude = &clis[&Provider::Claude];
        assert_eq!(claude.program, Path::new("claude"));
        assert_eq!(claude.model, "haiku");
        assert_eq!(claude.config_dir_env, "CLAUDE_CONFIG_DIR");
        assert!(claude.args.starts_with(&["-p".to_owned()]));
        let codex = &clis[&Provider::Codex];
        assert_eq!(codex.program, Path::new("/opt/codex"));
        assert_eq!(codex.config_dir_env, "CODEX_HOME");
        assert!(codex.args.starts_with(&["exec".to_owned()]));
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
        let usage = Known {
            windows: BTreeMap::from([(AccountId::new("claude-main"), vec![window.clone()])]),
            emails: BTreeMap::from([(AccountId::new("claude-main"), "dev@example.com".into())]),
        };
        assert_eq!(
            list(&accounts, &usage),
            [
                Account {
                    config_dir: Some("/secret/dir".into()),
                    account_id: AccountId::new("claude-main"),
                    provider: Provider::Claude,
                    label: "Label".into(),
                    email: Some("dev@example.com".into()),
                    usage: vec![window],
                },
                Account {
                    config_dir: None,
                    account_id: AccountId::new("codex"),
                    provider: Provider::Codex,
                    label: "Label".into(),
                    email: None,
                    usage: Vec::new(),
                }
            ]
        );
    }

    #[test]
    fn probes_cover_claude_and_codex_only() {
        let probes = probes(&HashMap::new());
        let mut providers: Vec<_> = probes.keys().map(Provider::as_str).collect();
        providers.sort_unstable();
        assert_eq!(providers, ["claude", "codex"]);
    }
}
