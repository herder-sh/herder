//! Provider CLIs on this host: whether each one runs, its `--version`, the newest release its
//! vendor publishes, and the owner-only install or update that runs in a relayed terminal.
//!
//! The probe is the same `--version` [`herder doctor`](../../herder/src/doctor.rs) already
//! runs. Install never runs silently: the owner watches the vendor's documented installer or
//! updater. An installed CLI is updated the way it was installed, as its binary's resolved
//! path tells: a Homebrew formula or cask with `brew upgrade`, a global npm or bun package with
//! that package manager, anything else with the CLI's own updater. herder never reads a
//! provider's config dir.

use std::collections::HashMap;
use std::io::{ErrorKind, Read};
use std::path::{Component, Path, PathBuf};
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

/// How often the daemon asks the vendors for their newest releases; first one [`INTERVAL`]
/// after it starts.
pub const LATEST_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

/// How long a CLI's `--version`, or its own update check, may take.
const VERSION_TIMEOUT: Duration = Duration::from_secs(10);

/// Where the npm packages of Claude Code, Codex and opencode are published.
const NPM_REGISTRY: &str = "https://registry.npmjs.org";

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
    /// The newest release of each provider, as last checked.
    latest: Mutex<HashMap<Provider, String>>,
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
            latest: Mutex::new(HashMap::new()),
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
        let latest = lock(&self.inner.latest).clone();
        let current = lock(&self.inner.current).clone();
        let hub = lock(&self.inner.hub).clone();
        let providers = Self {
            inner: Arc::new(Inner {
                binaries: self.inner.binaries.clone(),
                recipes,
                latest: Mutex::new(latest),
                current: Mutex::new(current),
                hub: Mutex::new(hub),
            }),
        };
        providers.refresh();
        providers
    }

    /// Reports `version` as `provider`'s newest release, as a check of its vendor would; for
    /// tests and the demo, which reach no vendor.
    pub fn with_latest(self, provider: Provider, version: &str) -> Self {
        lock(&self.inner.latest).insert(provider, version.to_owned());
        self.refresh();
        self
    }

    /// Announces later probes through `hub`.
    pub fn publish_to(&self, hub: Arc<Hub>) {
        *lock(&self.inner.hub) = Some(hub);
        self.publish();
    }

    /// The latest probe.
    pub fn snapshot(&self) -> Vec<ProviderStatus> {
        lock(&self.inner.current).clone()
    }

    /// Re-runs `--version` for each runnable provider and announces a change.
    pub fn refresh(&self) {
        let latest = lock(&self.inner.latest).clone();
        let next = probe(&self.inner.binaries, &self.inner.recipes, &latest);
        let mut current = lock(&self.inner.current);
        if *current == next {
            return;
        }
        *current = next;
        drop(current);
        self.publish();
    }

    /// Asks the vendors of the installed providers for their newest releases, keeping the last
    /// answer of one that cannot be asked now, and announces a change. Blocks on the network.
    fn check_latest(&self, registry: &str) {
        let installed: Vec<Provider> = self
            .snapshot()
            .into_iter()
            .filter(|status| status.installed)
            .map(|status| status.provider)
            .collect();
        for provider in installed {
            let program = accounts::program(&provider, &self.inner.binaries);
            match latest_release(&provider, program.as_deref(), registry) {
                Ok(Some(version)) => {
                    lock(&self.inner.latest).insert(provider, version);
                }
                Ok(None) => {}
                Err(err) => debug!(
                    provider = provider.as_str(),
                    "cannot check the newest release: {err}"
                ),
            }
        }
        self.refresh();
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
                "herder has no install recipe for {} on this host",
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

    /// Probes on `INTERVAL`, and checks the newest releases on `LATEST_INTERVAL`, until
    /// `shutdown`.
    pub async fn run(&self, shutdown: tokio_util::sync::CancellationToken) {
        let start = tokio::time::Instant::now() + INTERVAL;
        let mut latest = tokio::time::interval_at(start, LATEST_INTERVAL);
        loop {
            tokio::select! {
                () = shutdown.cancelled() => return,
                () = tokio::time::sleep(INTERVAL) => self.refresh(),
                _ = latest.tick() => {
                    let providers = self.clone();
                    let checked = tokio::task::spawn_blocking(move || {
                        providers.check_latest(NPM_REGISTRY);
                    });
                    if let Err(err) = checked.await {
                        debug!("the newest-release check failed: {err}");
                    }
                }
            }
        }
    }

    fn publish(&self) {
        let Some(hub) = lock(&self.inner.hub).clone() else {
            return;
        };
        hub.providers_changed(&self.snapshot());
    }
}

/// `--version` of each runnable provider, whether herder can install or update it, and
/// whether `latest` is newer.
pub fn probe(
    binaries: &HashMap<Provider, PathBuf>,
    recipes: &HashMap<Provider, InstallRecipe>,
    latest: &HashMap<Provider, String>,
) -> Vec<ProviderStatus> {
    accounts::PROVIDERS
        .iter()
        .map(|provider| status(provider, binaries, recipes, latest.get(provider)))
        .collect()
}

fn status(
    provider: &Provider,
    binaries: &HashMap<Provider, PathBuf>,
    recipes: &HashMap<Provider, InstallRecipe>,
    latest: Option<&String>,
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
    let latest_version = match (&version, latest) {
        (Some(version), Some(latest)) if newer(latest, version) => Some(latest.clone()),
        _ => None,
    };
    let can_install = recipe(provider, false, binaries, recipes).is_some();
    let can_update = installed && recipe(provider, true, binaries, recipes).is_some();
    ProviderStatus {
        provider: provider.clone(),
        installed,
        version,
        latest_version,
        binary: shown,
        can_install,
        can_update,
    }
}

fn version_of(program: &Path) -> std::io::Result<String> {
    let text = output_of(program, &["--version"])?;
    Ok(text.lines().next().unwrap_or("").trim().to_owned())
}

/// What `program` with `args` prints, if it succeeds within [`VERSION_TIMEOUT`].
fn output_of(program: &Path, args: &[&str]) -> std::io::Result<String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let shown = || format!("`{} {}`", program.display(), args.join(" "));
    let started = std::time::Instant::now();
    loop {
        match child.try_wait()? {
            Some(status) => {
                let output = child.wait_with_output()?;
                if !status.success() {
                    return Err(std::io::Error::other(format!(
                        "{} failed ({status})",
                        shown()
                    )));
                }
                return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
            }
            None if started.elapsed() >= VERSION_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(std::io::Error::other(format!("{} timed out", shown())));
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}

/// The newest release of `provider` its vendor publishes: Claude Code, Codex and opencode
/// from their npm packages in `registry`, Grok from its own `grok update --check`. `None` for
/// Cursor, which publishes no version a client can ask for.
fn latest_release(
    provider: &Provider,
    program: Option<&Path>,
    registry: &str,
) -> Result<Option<String>, String> {
    let package = match provider {
        Provider::Claude => "@anthropic-ai/claude-code",
        Provider::Codex => "@openai/codex",
        Provider::Opencode => "opencode-ai",
        Provider::Grok => {
            let Some(program) = program else {
                return Ok(None);
            };
            let text = output_of(program, &["update", "--check", "--json"])
                .map_err(|err| err.to_string())?;
            let answer: serde_json::Value = serde_json::from_str(text.trim())
                .map_err(|err| format!("grok's update check is not JSON: {err}"))?;
            return Ok(answer["latestVersion"].as_str().map(str::to_owned));
        }
        Provider::Cursor | Provider::Gemini | Provider::Other(_) => return Ok(None),
    };
    let url = format!("{}/{package}/latest", registry.trim_end_matches('/'));
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(VERSION_TIMEOUT))
        .build()
        .into();
    let mut body = String::new();
    agent
        .get(&url)
        .call()
        .map_err(|err| format!("cannot reach {url}: {err}"))?
        .body_mut()
        .as_reader()
        .take(1024 * 1024)
        .read_to_string(&mut body)
        .map_err(|err| format!("cannot read {url}: {err}"))?;
    let answer: serde_json::Value =
        serde_json::from_str(&body).map_err(|err| format!("{url} is not JSON: {err}"))?;
    Ok(answer["version"].as_str().map(str::to_owned))
}

/// Whether `latest` is a newer release than `installed`, a CLI's `--version` line such as
/// `2.1.0 (Claude Code)` or `codex-cli 0.46.0`; false when either holds no version.
fn newer(latest: &str, installed: &str) -> bool {
    match (semver_in(latest), semver_in(installed)) {
        (Some(latest), Some(installed)) => latest > installed,
        _ => false,
    }
}

fn semver_in(text: &str) -> Option<semver::Version> {
    text.split_whitespace()
        .find_map(|word| semver::Version::parse(word.trim_start_matches('v')).ok())
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
    if installed {
        let program = accounts::program(provider, binaries)?;
        return update(provider, &program);
    }
    match provider {
        Provider::Claude => Some(shell("curl -fsSL https://claude.ai/install.sh | bash")),
        Provider::Codex => on_path("npm")
            .map(|_| shell("npm install -g @openai/codex@latest"))
            .or_else(|| on_path("brew").map(|_| shell("brew install --cask codex"))),
        Provider::Cursor => Some(shell("curl -fsSL https://cursor.com/install | bash")),
        Provider::Grok => Some(shell("curl -fsSL https://x.ai/cli/install.sh | bash")),
        Provider::Opencode => Some(shell("curl -fsSL https://opencode.ai/install | bash")),
        Provider::Gemini | Provider::Other(_) => None,
    }
}

/// The update of `provider` installed as `program`, the way it was installed.
fn update(provider: &Provider, program: &Path) -> Option<InstallRecipe> {
    let found = if program.components().count() > 1 {
        program.to_path_buf()
    } else {
        on_path(program.to_str()?)?
    };
    let resolved = found.canonicalize().ok()?;
    if let Some(recipe) = homebrew(&resolved).or_else(|| node_global(&resolved)) {
        return Some(recipe);
    }
    let updater = match provider {
        Provider::Claude | Provider::Codex | Provider::Cursor | Provider::Grok => "update",
        Provider::Opencode => "upgrade",
        Provider::Gemini | Provider::Other(_) => return None,
    };
    Some(InstallRecipe {
        program: found,
        args: vec![updater.to_owned()],
    })
}

/// `brew upgrade` of the formula or cask whose keg holds `resolved`, by the `brew` of its own
/// Homebrew prefix: `<prefix>/Cellar/<formula>/…` or `<prefix>/Caskroom/<cask>/…`.
fn homebrew(resolved: &Path) -> Option<InstallRecipe> {
    let parts: Vec<Component> = resolved.components().collect();
    let at = parts
        .iter()
        .position(|part| matches!(part.as_os_str().to_str(), Some("Cellar" | "Caskroom")))?;
    let name = parts.get(at + 1)?.as_os_str().to_str()?;
    let brew: PathBuf = parts[..at].iter().collect::<PathBuf>().join("bin/brew");
    if !brew.is_file() {
        return None;
    }
    let mut args = vec!["upgrade".to_owned()];
    if parts[at].as_os_str() == "Caskroom" {
        args.push("--cask".to_owned());
    }
    args.push(name.to_owned());
    Some(InstallRecipe {
        program: brew,
        args,
    })
}

/// The global reinstall at `@latest` of the npm package that holds `resolved`, by the package
/// manager that installed it: npm's `<prefix>/lib/node_modules/<package>/…`, or bun's
/// `~/.bun/install/global/node_modules/<package>/…`.
fn node_global(resolved: &Path) -> Option<InstallRecipe> {
    let parts: Vec<&str> = resolved
        .components()
        .map(|part| part.as_os_str().to_str())
        .collect::<Option<_>>()?;
    let at = parts.iter().position(|part| *part == "node_modules")?;
    let package = match parts.get(at + 1)? {
        scope if scope.starts_with('@') => format!("{scope}/{}", parts.get(at + 2)?),
        name => (*name).to_owned(),
    };
    let root = |end: usize| parts[..end].iter().collect::<PathBuf>();
    let (bin, install) = if at >= 3 && parts[at - 3..at] == [".bun", "install", "global"] {
        (root(at - 2).join("bin"), "bun add -g")
    } else if at >= 1 && parts[at - 1] == "lib" {
        (root(at - 1).join("bin"), "npm install -g")
    } else {
        return None;
    };
    let manager = install.split(' ').next()?;
    if !bin.join(manager).is_file() {
        return None;
    }
    // The package manager's own directory comes first on `PATH`: npm runs on the `node`
    // installed beside it, as under nvm.
    Some(InstallRecipe {
        program: PathBuf::from("sh"),
        args: vec![
            "-c".to_owned(),
            format!("PATH=\"$0:$PATH\" exec {install} \"$1\""),
            bin.display().to_string(),
            format!("{package}@latest"),
        ],
    })
}

/// `name` on the daemon's `PATH`.
fn on_path(name: &str) -> Option<PathBuf> {
    crate::aws::find(name, &std::env::var_os("PATH")?)
}

fn shell(script: &str) -> InstallRecipe {
    InstallRecipe {
        program: PathBuf::from("sh"),
        args: vec!["-c".to_owned(), script.to_owned()],
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    // Every update is a single assignment or insert, so a poisoned lock is still consistent.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
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
        std::fs::create_dir_all(dir).unwrap();
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
        let statuses = probe(&binaries, &HashMap::new(), &HashMap::new());
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
        let statuses = probe(&binaries, &HashMap::new(), &HashMap::new());
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
    fn grok_installs_with_the_xai_installer() {
        let tmp = tempfile::tempdir().unwrap();
        let binaries = HashMap::from([(Provider::Grok, tmp.path().join("no-such-grok"))]);
        let providers = Providers::new(binaries);
        let recipe = providers.recipe(&Provider::Grok).unwrap();
        assert_eq!(
            recipe,
            shell("curl -fsSL https://x.ai/cli/install.sh | bash")
        );
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

    /// A CLI at `real`, linked from `link` the way its installer links it onto `PATH`.
    fn linked(real: &Path, link: &Path, body: &str) -> PathBuf {
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        let name = real.file_name().unwrap().to_str().unwrap();
        script(real.parent().unwrap(), name, body);
        std::os::unix::fs::symlink(real, link).unwrap();
        link.to_path_buf()
    }

    const VERSION_1: &str = "#!/bin/sh\necho 1.0.0\n";

    #[test]
    fn a_homebrew_formula_or_cask_updates_with_its_own_brew() {
        let tmp = tempfile::tempdir().unwrap();
        let prefix = tmp.path().join("homebrew");
        let brew = script(&prefix.join("bin"), "brew", "#!/bin/sh\n");
        let opencode = linked(
            &prefix.join("Cellar/opencode/1.0.0/bin/opencode"),
            &prefix.join("bin/opencode"),
            VERSION_1,
        );
        let claude = linked(
            &prefix.join("Caskroom/claude-code/1.0.0/claude"),
            &prefix.join("bin/claude"),
            VERSION_1,
        );
        let brew = brew.canonicalize().unwrap();
        assert_eq!(
            update(&Provider::Opencode, &opencode),
            Some(InstallRecipe {
                program: brew.clone(),
                args: vec!["upgrade".into(), "opencode".into()],
            })
        );
        assert_eq!(
            update(&Provider::Claude, &claude),
            Some(InstallRecipe {
                program: brew,
                args: vec!["upgrade".into(), "--cask".into(), "claude-code".into()],
            })
        );
    }

    #[test]
    fn a_global_npm_or_bun_package_updates_with_its_package_manager() {
        let tmp = tempfile::tempdir().unwrap();
        // npm's directory as the update names it: resolved, as macOS's temp dir is a link.
        let prefix = tmp.path().canonicalize().unwrap().join("node");
        script(&prefix.join("bin"), "npm", "#!/bin/sh\n");
        let codex = linked(
            &prefix.join("lib/node_modules/@openai/codex/bin/codex.js"),
            &prefix.join("bin/codex"),
            VERSION_1,
        );
        assert_eq!(
            update(&Provider::Codex, &codex),
            Some(InstallRecipe {
                program: PathBuf::from("sh"),
                args: vec![
                    "-c".into(),
                    "PATH=\"$0:$PATH\" exec npm install -g \"$1\"".into(),
                    prefix.join("bin").display().to_string(),
                    "@openai/codex@latest".into(),
                ],
            })
        );

        let bun = prefix.join(".bun");
        script(&bun.join("bin"), "bun", "#!/bin/sh\n");
        let opencode = linked(
            &bun.join("install/global/node_modules/opencode-ai/bin/opencode"),
            &bun.join("bin/opencode"),
            VERSION_1,
        );
        let recipe = update(&Provider::Opencode, &opencode).unwrap();
        assert_eq!(recipe.args[1], "PATH=\"$0:$PATH\" exec bun add -g \"$1\"");
        assert_eq!(recipe.args[2], bun.join("bin").display().to_string());
        assert_eq!(recipe.args[3], "opencode-ai@latest");
    }

    #[test]
    fn a_native_install_updates_with_the_clis_own_updater() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        for (provider, real, link, updater) in [
            (
                Provider::Claude,
                ".local/share/claude/versions/1.0.0",
                ".local/bin/claude",
                "update",
            ),
            (
                Provider::Cursor,
                ".local/share/cursor-agent/versions/1/cursor-agent",
                ".local/bin/cursor-agent",
                "update",
            ),
            (
                Provider::Codex,
                ".codex/releases/1/bin/codex",
                ".local/bin/codex",
                "update",
            ),
            (
                Provider::Grok,
                ".grok/downloads/grok-1",
                ".grok/bin/grok",
                "update",
            ),
            (
                Provider::Opencode,
                ".opencode/bin/opencode",
                ".local/bin/opencode",
                "upgrade",
            ),
        ] {
            let program = linked(&home.join(real), &home.join(link), VERSION_1);
            assert_eq!(
                update(&provider, &program),
                Some(InstallRecipe {
                    program: program.clone(),
                    args: vec![updater.into()],
                }),
                "{provider:?}"
            );
        }
        // An npm package without npm beside it is the CLI's to update.
        let codex = linked(
            &home.join("volta/lib/node_modules/@openai/codex/bin/codex.js"),
            &home.join("volta/bin/codex"),
            VERSION_1,
        );
        assert_eq!(update(&Provider::Codex, &codex).unwrap().args, ["update"]);
    }

    #[test]
    fn the_update_recipe_runs_the_clis_updater() {
        let tmp = tempfile::tempdir().unwrap();
        let cli = tmp.path().join("opencode");
        let body = format!(
            "#!/bin/sh\ncase \"$1\" in\n--version) echo 1.0.0 ;;\nupgrade) printf '#!/bin/sh\\necho 1.1.0\\n' > '{}' ;;\n*) exit 1 ;;\nesac\n",
            cli.display()
        );
        script(tmp.path(), "opencode", &body);
        let providers = Providers::new(HashMap::from([(Provider::Opencode, cli)]));
        let recipe = providers.recipe(&Provider::Opencode).unwrap();
        assert_eq!(recipe.args, ["upgrade"]);
        let status = Command::new(&recipe.program)
            .args(&recipe.args)
            .status()
            .unwrap();
        assert!(status.success());
        providers.refresh();
        let opencode = providers
            .snapshot()
            .into_iter()
            .find(|status| status.provider == Provider::Opencode)
            .unwrap();
        assert_eq!(opencode.version.as_deref(), Some("1.1.0"));
    }

    #[test]
    fn newer_compares_the_versions_in_version_lines() {
        assert!(newer("2.1.4", "2.1.0 (Claude Code)"));
        assert!(newer("0.47.0", "codex-cli 0.46.0"));
        assert!(newer("1.0.63", "grok 1.0.62 (1474f9cbdd91)"));
        assert!(!newer("2.1.0", "2.1.0 (Claude Code)"));
        assert!(!newer("2.0.9", "v2.1.0"));
        assert!(!newer("2026.10.01", "2026.10.01-e373342"));
        assert!(!newer("garbage", "1.0.0"));
    }

    /// A registry on localhost that answers every request with `body`, once per request.
    fn registry(body: &'static str) -> String {
        use std::io::Write;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let mut request = [0; 4096];
                let _ = std::io::Read::read(&mut stream, &mut request);
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        format!("http://{address}")
    }

    #[test]
    fn the_newest_release_comes_from_npm_or_grok() {
        let registry = registry(r#"{"name":"@openai/codex","version":"0.47.0"}"#);
        assert_eq!(
            latest_release(&Provider::Codex, None, &registry),
            Ok(Some("0.47.0".into()))
        );
        assert_eq!(latest_release(&Provider::Cursor, None, &registry), Ok(None));

        let tmp = tempfile::tempdir().unwrap();
        let grok = script(
            tmp.path(),
            "grok",
            "#!/bin/sh\n[ \"$*\" = 'update --check --json' ] || exit 1\necho '{\"currentVersion\":\"1.0.62\",\"latestVersion\":\"1.0.63\",\"updateAvailable\":true}'\n",
        );
        assert_eq!(
            latest_release(&Provider::Grok, Some(&grok), "http://127.0.0.1:9"),
            Ok(Some("1.0.63".into()))
        );
    }

    #[test]
    fn a_newer_release_is_reported_until_installed() {
        let tmp = tempfile::tempdir().unwrap();
        let claude = script(
            tmp.path(),
            "claude",
            "#!/bin/sh\necho '2.1.0 (Claude Code)'\n",
        );
        let providers = Providers::new(HashMap::from([(Provider::Claude, claude)]));
        let claude = |providers: &Providers| {
            providers
                .snapshot()
                .into_iter()
                .find(|status| status.provider == Provider::Claude)
                .unwrap()
        };
        assert_eq!(claude(&providers).latest_version, None);

        providers.check_latest(&registry(r#"{"version":"2.1.4"}"#));
        assert_eq!(claude(&providers).latest_version.as_deref(), Some("2.1.4"));

        // An unreachable registry keeps the last answer.
        providers.check_latest("http://127.0.0.1:9");
        assert_eq!(claude(&providers).latest_version.as_deref(), Some("2.1.4"));

        providers.check_latest(&registry(r#"{"version":"2.1.0"}"#));
        assert_eq!(claude(&providers).latest_version, None);
    }
}
