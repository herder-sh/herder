//! Daemon runtime: the WebSocket server and the agent sessions it hosts.

pub mod accounts;
pub mod auth;
mod browse;
pub mod config;
pub mod data_dir;
pub mod handoff;
pub mod hub;
mod listen;
pub mod logging;
pub mod login;
pub mod mcp;
pub mod projects;
pub mod prs;
pub mod resources;
pub mod session;
pub mod settings;
pub mod skills;
pub mod stalls;
pub mod terminal;
pub mod usage;
pub mod vault;
pub mod worktree;
pub mod ws;

use std::sync::Arc;

use anyhow::{Context, Result};
use herder_protocol::HostId;
use tokio::signal::unix::{SignalKind, signal};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

pub use config::Config;
pub use data_dir::DataDir;
pub use hub::Hub;

/// How the daemon stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// On SIGTERM or Ctrl-C.
    Stopped,
    /// An owner asked it to start again ([`settings`]).
    Restart,
}

/// Runs the daemon until SIGTERM, Ctrl-C or an owner's restart, then shuts down cleanly.
pub fn run(config: Config) -> Result<Exit> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting the tokio runtime")?
        .block_on(async {
            let shutdown = CancellationToken::new();
            let mut sigterm =
                signal(SignalKind::terminate()).context("installing the SIGTERM handler")?;
            let token = shutdown.clone();
            tokio::spawn(async move {
                tokio::select! {
                    _ = sigterm.recv() => info!("received SIGTERM, shutting down"),
                    result = tokio::signal::ctrl_c() => match result {
                        Ok(()) => info!("received Ctrl-C, shutting down"),
                        Err(err) => error!("cannot listen for Ctrl-C, shutting down: {err}"),
                    },
                }
                token.cancel();
            });
            if config.mode == config::Mode::Vault {
                vault::serve(&config, shutdown).await?;
            } else {
                let adapters = accounts::adapters(&config.binaries);
                let probes = accounts::probes(&config.binaries);
                serve(&config, adapters, probes, config.accounts.clone(), shutdown).await?;
            }
            Ok(if settings::restart_requested() {
                Exit::Restart
            } else {
                Exit::Stopped
            })
        })
}

/// Opens the data dir, the journal and the TLS identity, then serves clients until `shutdown`,
/// running sessions on `accounts` through `adapters` and reading their usage with `probes`.
pub async fn serve(
    config: &Config,
    adapters: session::Adapters,
    probes: usage::Probes,
    accounts: session::Accounts,
    shutdown: CancellationToken,
) -> Result<()> {
    let data_dir = DataDir::open(&config.data_dir)?;
    let host = ws::Host {
        id: HostId::new(data_dir.host_id().to_string()),
        name: host_name(),
    };
    let tls = ws::Tls::load_or_create(&data_dir.root().join("tls"), &host.name)?;
    let store_path = data_dir.root().join("db/herder.db");
    let store = herder_store::Store::open(&store_path)
        .with_context(|| format!("opening the journal {}", store_path.display()))?;
    let auth = Arc::new(auth::Auth::open(data_dir.root())?);
    for (id, account) in &accounts {
        match &account.config_dir {
            Some(dir) if !dir.is_dir() => warn!(
                account_id = %id,
                "config dir {} does not exist yet; log in to {} there first",
                dir.display(),
                account.provider.as_str()
            ),
            _ => info!(account_id = %id, provider = account.provider.as_str(), "account loaded"),
        }
    }
    let hub = Arc::new(Hub::default());
    let terminals = terminal::Terminals::new(Arc::clone(&hub), terminal::login_shell());
    let sessions_changed = Arc::new(tokio::sync::Notify::new());
    let mut sink: Arc<dyn session::EventSink> = Arc::new(terminal::KillOnArchive {
        next: Arc::new(projects::OnSessionsChanged {
            next: Arc::clone(&hub) as Arc<dyn session::EventSink>,
            notify: Arc::clone(&sessions_changed),
        }),
        terminals: terminals.clone(),
    });
    // Always woken, as an owner may link a vault while the daemon runs.
    let journal_grew = Arc::new(tokio::sync::Notify::new());
    sink = Arc::new(vault::WakeOnEvent {
        next: sink,
        notify: Arc::clone(&journal_grew),
    });
    let setup = session::Setup {
        store,
        adapters,
        accounts,
        sink,
        turn_ids: session::ulid_turn_ids(),
        worktrees: worktree::Worktrees::new(data_dir.root().join("worktrees")),
        attachments: data_dir.root().join("attachments"),
    };
    let sessions = session::SessionManager::open(setup, shutdown.clone()).await?;
    let projects = Arc::new(projects::Overrides::new(
        config.path.clone(),
        data_dir.root().join("project-icons"),
        config.projects.clone(),
    ));
    sessions.manage_projects(host.id.clone(), Arc::clone(&projects))?;
    tokio::spawn(sessions.clone().sweep_archived_worktrees(shutdown.clone()));
    sessions.checkpoint_turns(worktree::checkpoint::Config {
        dir: data_dir.root().join("checkpoints"),
        keep: worktree::checkpoint::KEEP,
        push_timeout: worktree::checkpoint::PUSH_TIMEOUT,
    })?;
    let link = Arc::new(vault::Link::start(
        vault::LinkSetup {
            config_file: config.path.clone(),
            host: host.clone(),
            sessions: sessions.clone(),
            changed: journal_grew,
            data_dir: data_dir.root().to_owned(),
            shutdown: shutdown.clone(),
        },
        config.vault.clone(),
    )?);
    tokio::spawn(
        projects::Discovery {
            host: host.id.clone(),
            config: projects,
            hub: Arc::clone(&hub),
            sessions: sessions.clone(),
            sessions_changed,
        }
        .run(shutdown.clone()),
    );
    let scopes = Arc::new(resources::Scopes::detect(config.resources.clone()).await);
    sessions.limit_resources(Arc::clone(&scopes))?;
    sessions.configure_failover(config.failover.clone())?;
    sessions.generate_titles(
        config.titles.clone(),
        accounts::title_clis(&config.binaries),
    )?;
    let docker = Arc::new(resources::Docker::new("docker"));
    sessions.track_containers(Arc::clone(&docker))?;
    hub.set_failover(config.failover.settings());
    tokio::spawn({
        let hub = Arc::clone(&hub);
        let sessions = sessions.clone();
        let shutdown = shutdown.clone();
        let docker = Arc::clone(&docker);
        async move {
            let worktrees = || sessions.worktrees();
            resources::run_sampler(&scopes, &docker, worktrees, &hub, shutdown).await;
        }
    });
    let admission = Arc::new(resources::Admission::new(
        config.resources.budget(resources::cores()),
        Box::new(resources::ProcHost::default()),
    ));
    sessions.admit_turns(Arc::clone(&admission))?;
    let settings = Arc::new(settings::Settings::new(
        config,
        Some(Arc::clone(&admission)),
        shutdown.clone(),
    ));
    let budget = admission.budget();
    info!(
        max_turns = budget.max_turns,
        min_memory_available = budget.min_memory_available,
        max_memory_pressure = budget.max_memory_pressure,
        max_load = budget.max_load,
        "agent turns are admitted within this host's capacity"
    );
    tokio::spawn({
        let hub = Arc::clone(&hub);
        let shutdown = shutdown.clone();
        async move { admission.run(&hub, shutdown).await }
    });
    let herder = herder_binary()?;
    sessions.serve_mcp(
        mcp::Config {
            data_dir: data_dir.root().to_owned(),
            herder: herder.clone(),
        },
        config.tasks,
    )?;
    sessions
        .track_prs(prs::Config {
            data_dir: data_dir.root().to_owned(),
            herder,
            github: Arc::new(prs::GhCli),
            fast: prs::FAST,
            slow: prs::SLOW,
            follow_ups: config.follow_ups.pr_events,
        })
        .await?;
    sessions.watch_stalls(stalls::Config {
        after: std::time::Duration::from_secs(config.follow_ups.stall_after_secs),
        max_nudges: config.follow_ups.max_stall_nudges,
        pr_events: config.follow_ups.pr_events,
        interval: stalls::INTERVAL,
    })?;
    sessions.track_usage(usage::Config {
        probes,
        dir: data_dir.root().join("usage"),
        interval: usage::INTERVAL,
        fresh: usage::FRESH,
    })?;
    let skills = Arc::new(skills::Skills::open(
        data_dir.root(),
        &sessions.providers(),
        Arc::clone(&hub) as Arc<dyn skills::SkillsSink>,
    )?);
    sessions.deliver_skills(Arc::clone(&skills))?;
    let listing = sessions.clone();
    tokio::spawn(async move {
        skills.pull().await;
        if let Err(err) = listing.list_skills().await {
            warn!("cannot list the sessions' skills: {err:#}");
        }
    });
    sessions.resume().await?;
    let listeners = listen::bind(&config.listen).await?;
    let listen = listen::local_addrs(&listeners)?;
    let control = auth::control::bind(data_dir.root())?;
    tokio::spawn(auth::control::serve(
        control,
        Arc::clone(&auth),
        auth::control::Daemon {
            fingerprint: tls.fingerprint().to_owned(),
            listen: listen.clone(),
            sessions: Some(sessions.clone()),
            vault: None,
        },
        shutdown.clone(),
    ));
    info!(
        host_id = %data_dir.host_id(),
        data_dir = %data_dir.root().display(),
        listen = ?listen,
        tls_fingerprint = tls.fingerprint(),
        "herder daemon started"
    );
    let logins = login::Logins::new(
        login::programs(&config.binaries),
        config.path.clone(),
        sessions.clone(),
    );
    let server = ws::Server::new(tls, auth, hub, sessions, terminals.clone(), logins, host);
    server.link_vault(link)?;
    server.manage_settings(settings)?;
    server.run(listeners, shutdown).await;
    terminals.close_all();
    info!("herder daemon stopped");
    Ok(())
}

/// The running herder binary, for session git hooks to call. After an update replaced it on
/// disk, Linux reports the old path with ` (deleted)`; the new binary is at that path.
fn herder_binary() -> Result<std::path::PathBuf> {
    let exe = std::env::current_exe().context("locating the herder binary")?;
    let path = exe.to_string_lossy();
    Ok(match path.strip_suffix(" (deleted)") {
        Some(path) => path.into(),
        None => exe,
    })
}

/// This machine's host name, for display.
pub(crate) fn host_name() -> String {
    nix::unistd::gethostname()
        .ok()
        .and_then(|name| name.into_string().ok())
        .unwrap_or_else(|| "unknown".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn serve_returns_once_shutdown_is_cancelled() {
        let tmp = tempfile::tempdir().unwrap();
        let config = Config {
            path: tmp.path().join("daemon.toml"),
            listen: vec!["127.0.0.1:0".parse().unwrap()],
            data_dir: tmp.path().join("data"),
            log: config::LogConfig::default(),
            accounts: session::Accounts::new(),
            binaries: Default::default(),
            tasks: session::TaskLimits::default(),
            failover: Default::default(),
            titles: Default::default(),
            follow_ups: Default::default(),
            resources: Default::default(),
            projects: Default::default(),
            mode: config::Mode::Host,
            vault: None,
            retention: Default::default(),
        };
        let shutdown = CancellationToken::new();
        let task = tokio::spawn({
            let shutdown = shutdown.clone();
            async move {
                serve(
                    &config,
                    session::Adapters::new(),
                    usage::Probes::new(),
                    session::Accounts::new(),
                    shutdown,
                )
                .await
            }
        });
        shutdown.cancel();
        task.await.unwrap().unwrap();
        assert!(tmp.path().join("data/host-id").is_file());
        assert!(tmp.path().join("data/db/herder.db").is_file());
        assert!(tmp.path().join("data/tls/cert.pem").is_file());
        assert!(tmp.path().join("data/control.sock").exists());
        assert!(tmp.path().join("data/mcp.sock").exists());
    }
}
