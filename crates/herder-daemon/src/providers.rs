//! Provider CLIs on this host: whether each one runs, its `--version`, and the owner-only
//! install or update that runs in a relayed terminal.
//!
//! The probe is the same `--version` [`herder doctor`](../../herder/src/doctor.rs) already
//! runs. Install never runs silently: the owner watches the vendor's documented installer or
//! updater. herder never reads a provider's config dir.

use std::collections::HashMap;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use herder_protocol::{ErrorCode, ErrorInfo, Provider, ProviderStatus};
use portable_pty::CommandBuilder;
use tracing::debug;

use crate::accounts;
use crate::hub::Hub;

/// How often the daemon re-probes CLIs that may have appeared or been updated outside herder.
pub const INTERVAL: Duration = Duration::from_secs(60);

/// How long a CLI's `--version` may take.
const VERSION_TIMEOUT: Duration = Duration::from_secs(10);

/// The command an owner runs in a terminal to install or update a provider.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallRecipe {
    /// The program to run.
    pub program: PathBuf,
    /// Its arguments.
    pub args: Vec<String>,
}

/// The CLIs on this host and how to install them. Cheap to clone.
#[derive(Clone)]
pub struct Providers {
    inner: Arc<Inner>,
}

struct Inner {
    binaries: HashMap<Provider, PathBuf>,
    recipes: HashMap<Provider, InstallRecipe>,
    current: Mutex<Vec<ProviderStatus>>,
    hub: Mutex<Option<Arc<Hub>>>,
}

impl Providers {
    /// Probes the five runnable providers using `binaries` or each CLI on `PATH`.
    pub fn new(binaries: HashMap<Provider, PathBuf>) -> Self {
        let recipes = HashMap::new();
        let inner = Inner {
            binaries,
            recipes,
            current: Mutex::new(Vec::new()),
            hub: Mutex::new(None),
        };
        let providers = Self {
            inner: Arc::new(inner),
        };
        providers.refresh();
        providers
    }

    /// Uses `recipe` for `provider` instead of the documented vendor installer; for tests.
    pub fn with_recipe(self, provider: Provider, recipe: InstallRecipe) -> Self {
        let mut recipes = self.inner.recipes.clone();
        recipes.insert(provider, recipe);
        let current = self
            .inner
            .current
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let hub = self
            .inner
            .hub
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let providers = Self {
            inner: Arc::new(Inner {
                binaries: self.inner.binaries.clone(),
                recipes,
                current: Mutex::new(current),
                hub: Mutex::new(hub),
            }),
        };
        providers.refresh();
        providers
    }

    /// Announces later probes through `hub`.
    pub fn publish_to(&self, hub: Arc<Hub>) {
        *self
            .inner
            .hub
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(hub);
        self.publish();
    }

    /// The latest probe.
    pub fn snapshot(&self) -> Vec<ProviderStatus> {
        self.inner
            .current
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Re-runs `--version` for each runnable provider and announces a change.
    pub fn refresh(&self) {
        let next = probe(&self.inner.binaries, &self.inner.recipes);
        let mut current = self
            .inner
            .current
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if *current == next {
            return;
        }
        *current = next;
        drop(current);
        self.publish();
    }

    /// The documented installer or updater for `provider`, or why it cannot run.
    pub fn recipe(&self, provider: &Provider) -> Result<InstallRecipe, ErrorInfo> {
        if !accounts::runs(provider) {
            return Err(ErrorInfo {
                code: ErrorCode::BadRequest,
                message: format!("herder cannot run {} sessions", provider.as_str()),
            });
        }
        let status = self
            .snapshot()
            .into_iter()
            .find(|status| status.provider == *provider);
        let installed = status.as_ref().is_some_and(|status| status.installed);
        recipe(
            provider,
            installed,
            &self.inner.binaries,
            &self.inner.recipes,
        )
        .ok_or_else(|| ErrorInfo {
            code: ErrorCode::Unsupported,
            message: format!(
                "herder has no install recipe for {} on this OS",
                provider.as_str()
            ),
        })
    }

    /// The command to run in the install terminal.
    pub fn command(&self, provider: &Provider) -> Result<CommandBuilder, ErrorInfo> {
        let recipe = self.recipe(provider)?;
        let mut command = CommandBuilder::new(&recipe.program);
        for arg in &recipe.args {
            command.arg(arg);
        }
        Ok(command)
    }

    /// Probes on `INTERVAL` until `shutdown`.
    pub async fn run(&self, shutdown: tokio_util::sync::CancellationToken) {
        loop {
            tokio::select! {
                () = shutdown.cancelled() => return,
                () = tokio::time::sleep(INTERVAL) => self.refresh(),
            }
        }
    }

    fn publish(&self) {
        let Some(hub) = self
            .inner
            .hub
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
        else {
            return;
        };
        hub.providers_changed(&self.snapshot());
    }
}

/// `--version` of each runnable provider, and whether herder can install or update it.
pub fn probe(
    binaries: &HashMap<Provider, PathBuf>,
    recipes: &HashMap<Provider, InstallRecipe>,
) -> Vec<ProviderStatus> {
    accounts::PROVIDERS
        .iter()
        .map(|provider| status(provider, binaries, recipes))
        .collect()
}

fn status(
    provider: &Provider,
    binaries: &HashMap<Provider, PathBuf>,
    recipes: &HashMap<Provider, InstallRecipe>,
) -> ProviderStatus {
    let program = accounts::program(provider, binaries);
    let shown = program.as_ref().map(|path| path.display().to_string());
    let (installed, version) = match &program {
        Some(program) => match version_of(program) {
            Ok(version) => (true, Some(version)),
            Err(err) if err.kind() == ErrorKind::NotFound => (false, None),
            Err(err) => {
                debug!(provider = provider.as_str(), "{err}");
                (false, None)
            }
        },
        None => (false, None),
    };
    let can_install = recipe(provider, false, binaries, recipes).is_some();
    let can_update = installed && recipe(provider, true, binaries, recipes).is_some();
    ProviderStatus {
        provider: provider.clone(),
        installed,
        version,
        binary: shown,
        can_install,
        can_update,
    }
}

fn version_of(program: &std::path::Path) -> std::io::Result<String> {
    let mut child = Command::new(program)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let started = std::time::Instant::now();
    loop {
        match child.try_wait()? {
            Some(status) => {
                let output = child.wait_with_output()?;
                if !status.success() {
                    return Err(std::io::Error::other(format!(
                        "`{} --version` failed ({status})",
                        program.display()
                    )));
                }
                let text = String::from_utf8_lossy(&output.stdout);
                let line = text.lines().next().unwrap_or("").trim();
                return Ok(line.to_owned());
            }
            None if started.elapsed() >= VERSION_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(std::io::Error::other(format!(
                    "`{} --version` timed out",
                    program.display()
                )));
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}

fn recipe(
    provider: &Provider,
    installed: bool,
    binaries: &HashMap<Provider, PathBuf>,
    recipes: &HashMap<Provider, InstallRecipe>,
) -> Option<InstallRecipe> {
    if let Some(recipe) = recipes.get(provider) {
        return Some(recipe.clone());
    }
    if !cfg!(target_os = "linux") {
        return None;
    }
    match provider {
        Provider::Claude => Some(shell("curl -fsSL https://claude.ai/install.sh | bash")),
        Provider::Codex => Some(shell("npm install -g @openai/codex@latest")),
        Provider::Cursor if installed => {
            let program = accounts::program(provider, binaries)
                .unwrap_or_else(|| PathBuf::from("cursor-agent"));
            Some(InstallRecipe {
                program,
                args: vec!["update".into()],
            })
        }
        Provider::Cursor => Some(shell("curl -fsSL https://cursor.com/install | bash")),
        Provider::Opencode => Some(shell("curl -fsSL https://opencode.ai/install | bash")),
        Provider::Grok | Provider::Gemini | Provider::Other(_) => None,
    }
}

fn shell(script: &str) -> InstallRecipe {
    InstallRecipe {
        program: PathBuf::from("sh"),
        args: vec!["-c".into(), script.to_owned()],
    }
}

impl Default for Providers {
    fn default() -> Self {
        Self::new(HashMap::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn script(dir: &std::path::Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        path
    }

    #[test]
    fn probe_reads_version_from_a_fake_cli() {
        let tmp = tempfile::tempdir().unwrap();
        let path = script(
            tmp.path(),
            "claude",
            "#!/bin/sh\n[ \"$1\" = --version ] && { echo '2.1.0 (Claude Code)'; exit 0; }\nexit 1\n",
        );
        let binaries = HashMap::from([(Provider::Claude, path.clone())]);
        let statuses = probe(&binaries, &HashMap::new());
        let claude = statuses
            .iter()
            .find(|status| status.provider == Provider::Claude)
            .unwrap();
        assert!(claude.installed);
        assert_eq!(claude.version.as_deref(), Some("2.1.0 (Claude Code)"));
        assert_eq!(claude.binary.as_deref(), Some(path.to_str().unwrap()));
        assert!(claude.can_install);
        assert!(claude.can_update);
    }

    #[test]
    fn a_missing_cli_is_not_installed() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("no-such-claude");
        let binaries = HashMap::from([(Provider::Claude, missing)]);
        let statuses = probe(&binaries, &HashMap::new());
        let claude = statuses
            .iter()
            .find(|status| status.provider == Provider::Claude)
            .unwrap();
        assert!(!claude.installed);
        assert_eq!(claude.version, None);
        assert!(claude.can_install);
        assert!(!claude.can_update);
    }

    #[test]
    fn grok_has_no_recipe() {
        let providers = Providers::default();
        let error = providers.recipe(&Provider::Grok).unwrap_err();
        assert_eq!(error.code, ErrorCode::Unsupported);
        assert!(error.message.contains("no install recipe"));
    }

    #[test]
    fn gemini_is_not_runnable() {
        let providers = Providers::default();
        let error = providers.recipe(&Provider::Gemini).unwrap_err();
        assert_eq!(error.code, ErrorCode::BadRequest);
    }

    #[test]
    fn a_fake_installer_makes_the_cli_installed() {
        let tmp = tempfile::tempdir().unwrap();
        let cli = tmp.path().join("claude");
        let installer = script(
            tmp.path(),
            "install",
            &format!(
                "#!/bin/sh\nprintf '%s\\n' '#!/bin/sh' '[ \"$1\" = --version ] && {{ echo 9.9.9; exit 0; }}' 'exit 1' > '{}'\nchmod +x '{}'\n",
                cli.display(),
                cli.display()
            ),
        );
        let binaries = HashMap::from([(Provider::Claude, cli.clone())]);
        let providers = Providers::new(binaries).with_recipe(
            Provider::Claude,
            InstallRecipe {
                program: installer,
                args: Vec::new(),
            },
        );
        assert!(
            !providers
                .snapshot()
                .iter()
                .find(|status| status.provider == Provider::Claude)
                .unwrap()
                .installed
        );

        let recipe = providers.recipe(&Provider::Claude).unwrap();
        let status = Command::new(&recipe.program)
            .args(&recipe.args)
            .status()
            .unwrap();
        assert!(status.success());
        providers.refresh();
        let claude = providers
            .snapshot()
            .into_iter()
            .find(|status| status.provider == Provider::Claude)
            .unwrap();
        assert!(claude.installed);
        assert_eq!(claude.version.as_deref(), Some("9.9.9"));
    }
}
