//! Two real daemons, with the fake adapter, over TLS on localhost, each with a clone of one
//! repository: the client core pairs with both, and the session list shows one project with
//! the sessions of both machines.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use herder_adapters::fake::FakeAdapter;
use herder_client_core::PairingUri;
use herder_client_core::{Client, PairResult};
use herder_daemon::Hub;
use herder_daemon::auth::{Auth, PAIRING_TTL};
use herder_daemon::login::Logins;
use herder_daemon::projects::{Discovery, OnSessionsChanged, Overrides, ProjectsConfig};
use herder_daemon::session::{
    AccountConfig, Accounts, Adapters, EventSink, SessionManager, Setup, ulid_turn_ids,
};
use herder_daemon::terminal::Terminals;
use herder_daemon::worktree::Worktrees;
use herder_daemon::ws::{Host, Server, Tls};
use herder_protocol::{
    AccountId, CommandBody, HostId, PermissionMode, ProjectId, Provider, SessionHead,
};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use crate::app::{App, Msg, Row};

const TIMEOUT: Duration = Duration::from_secs(20);

fn account() -> AccountId {
    AccountId::new("account-1")
}

/// A git repository with one commit whose origin is `acme/app` on GitHub.
fn clone(dir: &Path) -> String {
    std::fs::create_dir_all(dir).unwrap();
    for args in [
        &["init", "--quiet", "--initial-branch=main"][..],
        &["commit", "--quiet", "--allow-empty", "-m", "init"],
        &["remote", "add", "origin", "git@github.com:acme/app.git"],
    ] {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=test", "-c", "user.email=test@example.com"])
            .args(args)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .status()
            .unwrap();
        assert!(status.success());
    }
    dir.to_str().unwrap().to_owned()
}

/// Starts a daemon named `name` on `dir`, with project discovery; returns a pairing link.
async fn daemon(dir: &Path, id: &str, name: &str, shutdown: &CancellationToken) -> String {
    std::fs::create_dir_all(dir.join("tls")).unwrap();
    let tls = Tls::load_or_create(&dir.join("tls"), name).unwrap();
    let auth = Arc::new(Auth::open(dir).unwrap());
    let hub = Arc::new(Hub::default());
    let fake = Provider::Other("fake".into());
    let mut adapters = Adapters::new();
    adapters.register(
        fake.clone(),
        Arc::new(FakeAdapter::new(dir.join("unused.jsonl"))),
    );
    let mut accounts = Accounts::new();
    accounts.insert(
        account(),
        AccountConfig {
            provider: fake,
            label: "Account 1".into(),
            config_dir: Some(dir.join("account")),
        },
    );
    let sessions_changed = Arc::new(tokio::sync::Notify::new());
    let setup = Setup {
        store: herder_store::Store::open(dir.join("herder.db")).unwrap(),
        adapters,
        accounts,
        sink: Arc::new(OnSessionsChanged {
            next: Arc::clone(&hub) as Arc<dyn EventSink>,
            notify: Arc::clone(&sessions_changed),
        }),
        turn_ids: ulid_turn_ids(),
        worktrees: Worktrees::new(dir.join("worktrees")),
        attachments: dir.join("attachments"),
    };
    let sessions = SessionManager::open(setup, shutdown.clone()).await.unwrap();
    let host = Host {
        id: HostId::new(id),
        name: name.into(),
    };
    tokio::spawn(
        Discovery {
            host: host.id.clone(),
            config: Arc::new(Overrides::new(
                dir.join("daemon.toml"),
                dir.join("project-icons"),
                ProjectsConfig::default(),
            )),
            hub: Arc::clone(&hub),
            sessions: sessions.clone(),
            sessions_changed,
        }
        .run(shutdown.clone()),
    );
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let fingerprint = tls.fingerprint().to_owned();
    let terminals = Terminals::new(Arc::clone(&hub), PathBuf::from("/bin/sh"));
    let logins = Logins::new(HashMap::new(), dir.join("daemon.toml"), sessions.clone());
    let server = Server::new(
        tls,
        Arc::clone(&auth),
        hub,
        sessions,
        terminals,
        logins,
        host,
    );
    tokio::spawn(server.run(vec![listener], shutdown.clone()));
    let code = auth.mint("alice", None, PAIRING_TTL).unwrap().code;
    PairingUri {
        hosts: vec![addr.to_string()],
        fingerprint,
        code,
    }
    .to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_paired_daemons_with_clones_of_one_repo_show_one_project() {
    let tmp = tempfile::tempdir().unwrap();
    let shutdown = CancellationToken::new();
    let machines = [
        ("host-1", "box", clone(&tmp.path().join("box/src/app"))),
        (
            "host-2",
            "laptop",
            clone(&tmp.path().join("laptop/work/app")),
        ),
    ];
    let client = Client::open(
        tmp.path().join("client").display().to_string(),
        "herder-tui/test".into(),
    )
    .unwrap();
    for (id, name, repo) in &machines {
        let link = daemon(&tmp.path().join(name), id, name, &shutdown).await;
        let paired = client.pair(link).await.unwrap();
        assert!(
            matches!(&paired[..], [PairResult::Paired { .. }]),
            "{paired:?}"
        );
        let command = CommandBody::CreateSession {
            repo: Some(repo.clone()),
            project_id: None,
            branch: None,
            account_id: Some(account()),
            provider: None,
            model: None,
            permission_mode: Some(PermissionMode::Ask),
            failover_pin: None,
        };
        client.send(HostId::new(*id), command).await.unwrap();
    }

    // Each daemon lists its session with the project its clone's remote names.
    let app_id = ProjectId::new("github.com/acme/app");
    let changes = client.changes();
    let resolved =
        |heads: &[SessionHead]| heads.len() == 1 && heads[0].project_id.as_ref() == Some(&app_id);
    tokio::time::timeout(TIMEOUT, async {
        while !client.machines().iter().all(|m| resolved(&m.sessions)) {
            assert!(changes.next().await);
        }
    })
    .await
    .expect("both daemons resolve their session's project");

    let mut app = App::default();
    app.update(Msg::Machines(client.machines()));
    let rows = app.rows();
    assert_eq!(rows.len(), 3, "{rows:?}");
    assert_eq!(rows[0], Row::Project(Some(app_id)));
    let mut hosts: Vec<&str> = rows[1..]
        .iter()
        .filter_map(Row::session)
        .map(|key| key.host_id.as_str())
        .collect();
    hosts.sort_unstable();
    assert_eq!(hosts, ["host-1", "host-2"]);
    shutdown.cancel();
}
