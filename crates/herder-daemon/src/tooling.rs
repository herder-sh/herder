//! Provider CLIs on this host: whether each runnable one is installed, its version, and
//! whether herder can run the vendor's installer or updater in a terminal.
//!
//! Status is probed with `<binary> --version`, the same check as `herder doctor`. Install and
//! update run the vendor's own documented command in a login-style terminal the owner watches;
//! herder never downloads a CLI itself. After that terminal exits, the set is probed again.

use std::collections::HashMap;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use herder_protocol::{ErrorCode, ErrorInfo, Provider, ProviderStatus};
use portable_pty::CommandBuilder;
use tokio::sync::Notify;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::accounts;
use crate::hub::Hub;

/// How often every runnable provider is probed.
pub const INTERVAL: Duration = Duration::from_secs(15 * 60);

/// How long a CLI's `--version` may take.
const VERSION_TIMEOUT: Duration = Duration::from_secs(10);

/// A vendor install or update, as a login shell command the owner watches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recipe {
    /// Installs the CLI when it is missing; also used to update when there is no updater.
    pub install: Option<String>,
    /// Updates an installed CLI; the installer when absent.
    pub update: Option<String>,
}

impl Recipe {
    fn install_cmd(&self) -> Option<&str> {
        self.install.as_deref()
    }

    fn update_cmd(&self) -> Option<&str> {
        self.update.as_deref().or(self.install.as_deref())
    }
}

/// Official vendor commands for this OS; `None` when herder has no recipe.
fn recipe(provider: &Provider) -> Recipe {
    match provider {
        Provider::Claude => Recipe {
            install: Some("curl -fsSL https://claude.ai/install.sh | bash".into()),
            update: None,
        },
        Provider::Codex => Recipe {
            install: Some("npm install -g @openai/codex".into()),
            update: None,
        },
        Provider::Cursor => Recipe {
            install: Some("curl https://cursor.com/install -fsS | bash".into()),
            update: Some("agent update".into()),
        },
        Provider::Opencode => Recipe {
            install: Some("curl -fsSL https://opencode.ai/install | bash".into()),
            update: None,
        },
        Provider::Grok | Provider::Gemini | Provider::Other(_) => Recipe {
            install: None,
            update: None,
        },
    }
}

/// Every runnable provider's CLI on this host. Cheap to clone.
#[derive(Clone)]
pub struct Tooling {
    inner: Arc<Inner>,
}

struct Inner {
    binaries: HashMap<Provider, PathBuf>,
    recipes: HashMap<Provider, Recipe>,
    hub: Arc<Hub>,
    current: Mutex<Vec<ProviderStatus>>,
    wake: Notify,
}

impl Tooling {
    /// Probes at once, then on an interval and whenever [`Tooling::refresh`] is asked.
    pub fn start(
        binaries: HashMap<Provider, PathBuf>,
        hub: Arc<Hub>,
        shutdown: CancellationToken,
    ) -> Self {
        Self::start_with(binaries, recipes(), hub, INTERVAL, shutdown)
    }

    /// As [`Tooling::start`], with recipes and interval for tests.
    pub fn start_with(
        binaries: HashMap<Provider, PathBuf>,
        recipes: HashMap<Provider, Recipe>,
        hub: Arc<Hub>,
        interval: Duration,
        shutdown: CancellationToken,
    ) -> Self {
        let inner = Arc::new(Inner {
            binaries,
            recipes,
            hub,
            current: Mutex::new(Vec::new()),
            wake: Notify::new(),
        });
        let tooling = Self {
            inner: Arc::clone(&inner),
        };
        tooling.publish(probe_all(&inner.binaries, &inner.recipes));
        tokio::spawn(async move { poll(inner, interval, shutdown).await });
        tooling
    }

    /// The last probe; empty until the first one lands.
    pub fn list(&self) -> Vec<ProviderStatus> {
        self.lock().clone()
    }

    /// Probes again as soon as the poller runs, however lately it last did.
    pub fn refresh(&self) {
        self.inner.wake.notify_one();
    }

    /// The command that installs or updates `provider` in a terminal, or why it cannot.
    pub fn command(&self, provider: &Provider) -> Result<CommandBuilder, ErrorInfo> {
        if !accounts::runs(provider) {
            return Err(error(
                ErrorCode::Unsupported,
                format!("herder cannot run {} sessions", provider.as_str()),
            ));
        }
        let recipe = self
            .inner
            .recipes
            .get(provider)
            .cloned()
            .unwrap_or_else(|| recipe(provider));
        let status = self
            .lock()
            .iter()
            .find(|status| status.provider == *provider)
            .cloned();
        let installed = status.as_ref().is_some_and(|status| status.installed);
        let script = if installed {
            recipe.update_cmd()
        } else {
            recipe.install_cmd()
        };
        let Some(script) = script else {
            return Err(error(
                ErrorCode::Unsupported,
                format!(
                    "herder has no {} installer for this machine",
                    provider.as_str()
                ),
            ));
        };
        let mut command = CommandBuilder::new("/bin/sh");
        command.arg("-lc");
        command.arg(script);
        if let Some(home) = std::env::var_os("HOME") {
            command.cwd(home);
        }
        Ok(command)
    }

    fn publish(&self, providers: Vec<ProviderStatus>) {
        publish(&self.inner, providers);
    }

    fn lock(&self) -> MutexGuard<'_, Vec<ProviderStatus>> {
        self.inner
            .current
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

fn recipes() -> HashMap<Provider, Recipe> {
    accounts::PROVIDERS
        .into_iter()
        .map(|provider| (provider.clone(), recipe(&provider)))
        .collect()
}

fn publish(inner: &Inner, providers: Vec<ProviderStatus>) {
    {
        let mut current = inner.current.lock().unwrap_or_else(PoisonError::into_inner);
        if *current == providers {
            return;
        }
        *current = providers.clone();
    }
    inner.hub.providers_changed(providers);
}

async fn poll(inner: Arc<Inner>, interval: Duration, shutdown: CancellationToken) {
    let mut due = Instant::now() + interval;
    loop {
        tokio::select! {
            () = shutdown.cancelled() => return,
            () = inner.wake.notified() => {}
            () = tokio::time::sleep_until(due) => {}
        }
        if shutdown.is_cancelled() {
            return;
        }
        let binaries = inner.binaries.clone();
        let recipes = inner.recipes.clone();
        let providers = tokio::task::spawn_blocking(move || probe_all(&binaries, &recipes)).await;
        match providers {
            Ok(providers) => publish(&inner, providers),
            Err(err) => warn!("cannot probe provider CLIs: {err}"),
        }
        due = Instant::now() + interval;
    }
}

fn probe_all(
    binaries: &HashMap<Provider, PathBuf>,
    recipes: &HashMap<Provider, Recipe>,
) -> Vec<ProviderStatus> {
    accounts::PROVIDERS
        .into_iter()
        .map(|provider| {
            let recipe = recipes
                .get(&provider)
                .cloned()
                .unwrap_or_else(|| recipe(&provider));
            probe(&provider, binaries, &recipe)
        })
        .collect()
}

fn probe(
    provider: &Provider,
    binaries: &HashMap<Provider, PathBuf>,
    recipe: &Recipe,
) -> ProviderStatus {
    let binary = accounts::program(provider, binaries);
    let shown = binary.as_ref().map(|path| path.display().to_string());
    let (installed, version) = match &binary {
        Some(program) => match version_of(program) {
            Ok(version) => (true, Some(version)),
            Err(err) if err.kind() == ErrorKind::NotFound => (false, None),
            Err(err) => {
                debug!(provider = provider.as_str(), "cannot run --version: {err}");
                (false, None)
            }
        },
        None => (false, None),
    };
    ProviderStatus {
        provider: provider.clone(),
        installed,
        version,
        binary: shown,
        can_install: recipe.install_cmd().is_some(),
        can_update: installed && recipe.update_cmd().is_some(),
    }
}

fn version_of(program: &Path) -> io::Result<String> {
    let mut child = Command::new(program)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let deadline = std::time::Instant::now() + VERSION_TIMEOUT;
    loop {
        if child.try_wait()?.is_some() {
            let out = child.wait_with_output()?;
            if !out.status.success() {
                return Err(io::Error::other(format!(
                    "`{} --version` failed ({})",
                    program.display(),
                    out.status
                )));
            }
            return Ok(first_line(&out));
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(
                ErrorKind::TimedOut,
                format!("no answer within {} s", VERSION_TIMEOUT.as_secs()),
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn first_line(out: &Output) -> String {
    [&out.stdout, &out.stderr]
        .into_iter()
        .flat_map(|bytes| {
            String::from_utf8_lossy(bytes)
                .lines()
                .map(str::trim)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .find(|line| !line.is_empty())
        .unwrap_or_default()
}

fn error(code: ErrorCode, message: String) -> ErrorInfo {
    ErrorInfo { code, message }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fake_cli(dir: &Path, name: &str, version: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\necho '{version}'\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perm = std::fs::metadata(&path).unwrap().permissions();
            perm.set_mode(0o755);
            std::fs::set_permissions(&path, perm).unwrap();
        }
        path
    }

    #[tokio::test]
    async fn probe_reports_installed_version_and_a_missing_cli() {
        let tmp = tempfile::tempdir().unwrap();
        let claude = fake_cli(tmp.path(), "claude", "2.1.0 (Claude Code)");
        let mut binaries = HashMap::new();
        binaries.insert(Provider::Claude, claude);
        binaries.insert(Provider::Cursor, tmp.path().join("agent-missing"));
        let hub = Arc::new(Hub::default());
        let shutdown = CancellationToken::new();
        let tooling = Tooling::start_with(
            binaries,
            recipes(),
            Arc::clone(&hub),
            Duration::from_secs(60),
            shutdown,
        );
        let by_name: HashMap<_, _> = tooling
            .list()
            .into_iter()
            .map(|status| (status.provider.clone(), status))
            .collect();
        let claude = &by_name[&Provider::Claude];
        assert!(claude.installed, "{claude:?}");
        assert!(
            claude
                .version
                .as_deref()
                .is_some_and(|v| v.contains("2.1.0")),
            "{claude:?}"
        );
        assert!(claude.can_install);
        assert!(claude.can_update);
        let cursor = &by_name[&Provider::Cursor];
        assert!(!cursor.installed, "{cursor:?}");
        assert!(cursor.can_install);
        assert!(!cursor.can_update);
        let grok = &by_name[&Provider::Grok];
        assert!(!grok.can_install);
        assert!(!grok.can_update);
    }

    #[tokio::test]
    async fn install_is_refused_without_a_recipe() {
        let hub = Arc::new(Hub::default());
        let tooling = Tooling::start_with(
            HashMap::new(),
            recipes(),
            hub,
            Duration::from_secs(60),
            CancellationToken::new(),
        );
        let err = tooling.command(&Provider::Grok).unwrap_err();
        assert_eq!(err.code, ErrorCode::Unsupported);
        assert!(err.message.contains("installer"), "{}", err.message);
        let err = tooling.command(&Provider::Gemini).unwrap_err();
        assert_eq!(err.code, ErrorCode::Unsupported);
    }

    #[tokio::test]
    async fn a_changed_probe_replaces_the_list() {
        let tmp = tempfile::tempdir().unwrap();
        let hub = Arc::new(Hub::default());
        let missing = tmp.path().join("missing");
        let mut binaries = HashMap::new();
        for provider in accounts::PROVIDERS {
            binaries.insert(provider.clone(), missing.clone());
        }
        let tooling = Tooling::start_with(
            binaries.clone(),
            recipes(),
            hub,
            Duration::from_secs(60),
            CancellationToken::new(),
        );
        assert!(
            tooling.list().iter().all(|status| !status.installed),
            "{:?}",
            tooling.list()
        );
        binaries.insert(Provider::Claude, fake_cli(tmp.path(), "claude", "9.9.9"));
        tooling.publish(probe_all(&binaries, &recipes()));
        let claude = tooling
            .list()
            .into_iter()
            .find(|status| status.provider == Provider::Claude)
            .unwrap();
        assert!(claude.installed);
        assert!(
            claude
                .version
                .as_deref()
                .is_some_and(|v| v.contains("9.9.9")),
            "{claude:?}"
        );
    }
}
