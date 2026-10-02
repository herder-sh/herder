use std::fs;

use super::*;

fn host() -> HostId {
    HostId::new("HOST")
}

fn repo(path: &str, origin: Option<&str>) -> Repo {
    Repo {
        path: PathBuf::from(path),
        origin: origin.map(str::to_owned),
    }
}

fn project(id: &str, name: &str, paths: &[&str]) -> Project {
    Project {
        project_id: ProjectId::new(id),
        name: name.to_owned(),
        paths: paths.iter().map(|p| (*p).to_owned()).collect(),
        default_account: None,
        setup_command: None,
    }
}

#[test]
fn two_forms_of_one_remote_are_one_project() {
    let repos = [
        repo("/src/a", Some("git@github.com:org/repo.git")),
        repo("/work/b", Some("https://user@GitHub.com/org/repo/")),
    ];
    assert_eq!(
        resolve(&host(), &repos, &[]),
        [project(
            "github.com/org/repo",
            "repo",
            &["/src/a", "/work/b"]
        )]
    );
}

#[test]
fn repos_without_a_remote_are_local_projects_of_this_host() {
    let repos = [
        repo("/src/scratch", None),
        repo("/src/files", Some("/mnt/bare/files.git")),
    ];
    assert_eq!(
        resolve(&host(), &repos, &[]),
        [
            project("HOST:/src/files", "files", &["/src/files"]),
            project("HOST:/src/scratch", "scratch", &["/src/scratch"]),
        ]
    );
}

#[test]
fn an_entry_renames_and_merges_remotes_with_its_settings() {
    let repos = [
        repo("/src/herder", Some("git@github.com:herder-sh/herder.git")),
        repo("/src/mirror", Some("https://gitlab.com/mirror/herder")),
        repo("/src/other", Some("https://github.com/org/other")),
    ];
    let entries = [ProjectEntry {
        name: Some("herder".to_owned()),
        remotes: vec![
            ProjectId::new("github.com/herder-sh/herder"),
            ProjectId::new("gitlab.com/mirror/herder"),
        ],
        default_account: Some(AccountId::new("claude-main")),
        setup_command: Some("make bootstrap".to_owned()),
        ..ProjectEntry::default()
    }];
    assert_eq!(
        resolve(&host(), &repos, &entries),
        [
            Project {
                default_account: Some(AccountId::new("claude-main")),
                setup_command: Some("make bootstrap".to_owned()),
                ..project(
                    "github.com/herder-sh/herder",
                    "herder",
                    &["/src/herder", "/src/mirror"]
                )
            },
            project("github.com/org/other", "other", &["/src/other"]),
        ]
    );
}

#[test]
fn a_path_declared_project_takes_its_id_from_the_first_path() {
    let repos = [
        repo("/srv/scratch", None),
        repo("/srv/notes", Some("https://example.com/me/notes.git")),
        repo("/home/dev/notes", Some("git@example.com:me/notes")),
    ];
    let entries = [
        ProjectEntry {
            name: Some("Scratch".to_owned()),
            paths: vec![PathBuf::from("/srv/missing"), PathBuf::from("/srv/scratch")],
            ..ProjectEntry::default()
        },
        // Declared by a path that has a remote: every clone of that remote joins it.
        ProjectEntry {
            name: Some("Notes".to_owned()),
            paths: vec![PathBuf::from("/srv/notes")],
            ..ProjectEntry::default()
        },
    ];
    assert_eq!(
        resolve(&host(), &repos, &entries),
        [
            project("HOST:/srv/scratch", "Scratch", &["/srv/scratch"]),
            project(
                "example.com/me/notes",
                "Notes",
                &["/home/dev/notes", "/srv/notes"]
            ),
        ]
    );
}

#[test]
fn a_listed_path_joins_the_entry_whatever_its_remote() {
    let repos = [
        repo("/src/app", Some("https://github.com/org/app")),
        repo("/src/app-fork", Some("https://github.com/me/app")),
    ];
    let entries = [ProjectEntry {
        remotes: vec![ProjectId::new("github.com/org/app")],
        paths: vec![PathBuf::from("/src/app-fork")],
        ..ProjectEntry::default()
    }];
    assert_eq!(
        resolve(&host(), &repos, &entries),
        [project(
            "github.com/org/app",
            "app",
            &["/src/app", "/src/app-fork"]
        )]
    );
}

#[test]
fn an_entry_without_clones_here_is_left_out() {
    let entries = [ProjectEntry {
        remotes: vec![ProjectId::new("github.com/org/elsewhere")],
        ..ProjectEntry::default()
    }];
    assert!(resolve(&host(), &[], &entries).is_empty());
}

fn git_repo(path: &Path, config: &str) {
    fs::create_dir_all(path.join(".git")).unwrap();
    fs::write(path.join(".git/config"), config).unwrap();
}

#[test]
fn the_roots_scan_finds_nested_repos_and_skips_excluded_dirs() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("Projects");
    for found in ["top", "org/repo", "org/group/deep"] {
        git_repo(&root.join(found), "");
    }
    for skipped in [
        "org/group/sub/too-deep",
        ".hidden/repo",
        "app/node_modules/dep",
        "crate/target/repo",
        // Inside a repository: not descended into.
        "top/vendor/inner",
    ] {
        git_repo(&root.join(skipped), "");
    }
    fs::create_dir_all(root.join("app/src")).unwrap();
    std::os::unix::fs::symlink(&root, root.join("org/loop")).unwrap();

    let mut found = scan::repos(&[root.clone(), tmp.path().join("missing")]);
    found.sort();
    let expected: Vec<PathBuf> = ["org/group/deep", "org/repo", "top"]
        .iter()
        .map(|p| root.join(p))
        .collect();
    assert_eq!(found, expected);
}

#[test]
fn origin_is_read_from_the_repo_config() {
    let tmp = tempfile::tempdir().unwrap();
    let main = tmp.path().join("main");
    git_repo(
        &main,
        "[core]\n\tbare = false\n[remote \"upstream\"]\n\turl = https://example.com/up\n\
         [remote \"origin\"]\n\turl = git@github.com:org/repo.git\n\
         \tfetch = +refs/heads/*:refs/remotes/origin/*\n",
    );
    assert_eq!(
        scan::origin(&main).as_deref(),
        Some("git@github.com:org/repo.git")
    );

    // A linked worktree's `.git` file points into the main repo, whose config it shares.
    let linked = tmp.path().join("linked");
    let git_dir = main.join(".git/worktrees/linked");
    fs::create_dir_all(&git_dir).unwrap();
    fs::write(git_dir.join("commondir"), "../..\n").unwrap();
    fs::create_dir_all(&linked).unwrap();
    fs::write(
        linked.join(".git"),
        format!("gitdir: {}\n", git_dir.display()),
    )
    .unwrap();
    assert_eq!(
        scan::origin(&linked).as_deref(),
        Some("git@github.com:org/repo.git")
    );

    let bare = tmp.path().join("bare");
    git_repo(&bare, "[core]\n\tbare = false\n");
    assert_eq!(scan::origin(&bare), None);
    assert_eq!(scan::origin(&tmp.path().join("missing")), None);
}

#[test]
fn origin_url_follows_git_config_syntax() {
    let cases = [
        ("[remote \"origin\"]\nurl = a\nurl = b\n", Some("b")),
        (
            "[REMOTE \"origin\"]\n  URL = \"x y\" # comment\n",
            Some("x y"),
        ),
        ("[remote.origin]\nurl=c ; comment\n", Some("c")),
        ("[remote \"origin\"] url = d\n", Some("d")),
        ("[remote \"Origin\"]\nurl = e\n", None),
        ("[remote \"origin\"]\n# url = f\n", None),
        ("[remote \"origin\"]\n[core]\nurl = g\n", None),
        ("[remote \"origin\"]\nurl = \"a\\\"b\"\n", Some("a\"b")),
    ];
    for (config, expected) in cases {
        assert_eq!(scan::origin_url(config).as_deref(), expected, "{config}");
    }
}

/// The next project list the hub sends to `outbox`, within five seconds.
async fn next_projects(outbox: &crate::hub::Outbox) -> Vec<Project> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match outbox.pop() {
                Some(herder_protocol::ServerMessage::Projects { projects }) => return projects,
                Some(_) => {}
                None => outbox.ready().await,
            }
        }
    })
    .await
    .expect("a project list")
}

#[tokio::test]
async fn discovery_publishes_the_list_and_updates_it_when_sessions_change() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("Projects");
    git_repo(
        &root.join("app"),
        "[remote \"origin\"]\n\turl = git@github.com:org/app.git\n",
    );
    let declared = tmp.path().join("notes");
    let hub = Arc::new(Hub::default());
    let outbox = Arc::new(crate::hub::Outbox::default());
    hub.connect(&outbox, herder_protocol::Role::Member);
    let shutdown = CancellationToken::new();
    let sessions = SessionManager::open(
        crate::session::Setup {
            store: herder_store::Store::open(tmp.path().join("herder.db")).unwrap(),
            adapters: crate::session::Adapters::new(),
            accounts: crate::session::Accounts::new(),
            sink: Arc::clone(&hub) as Arc<dyn EventSink>,
            turn_ids: crate::session::ulid_turn_ids(),
            worktrees: crate::worktree::Worktrees::new(tmp.path().join("worktrees")),
        },
        shutdown.clone(),
    )
    .await
    .unwrap();
    let sessions_changed = Arc::new(Notify::new());
    let task = tokio::spawn(
        Discovery {
            host: host(),
            config: ProjectsConfig {
                roots: vec![root.clone()],
                entries: vec![ProjectEntry {
                    name: Some("Notes".to_owned()),
                    paths: vec![declared.clone()],
                    ..ProjectEntry::default()
                }],
                ..ProjectsConfig::default()
            },
            hub: Arc::clone(&hub),
            sessions,
            sessions_changed: Arc::clone(&sessions_changed),
        }
        .run(shutdown.clone()),
    );

    let app = root.join("app").to_string_lossy().into_owned();
    assert_eq!(
        next_projects(&outbox).await,
        [project("github.com/org/app", "app", &[&app])]
    );

    // A look at the sessions also picks up declared paths that appeared since.
    fs::create_dir_all(&declared).unwrap();
    sessions_changed.notify_one();
    let notes = declared.to_string_lossy().into_owned();
    assert_eq!(
        next_projects(&outbox).await,
        [
            project(&format!("HOST:{notes}"), "Notes", &[&notes]),
            project("github.com/org/app", "app", &[&app]),
        ]
    );

    shutdown.cancel();
    task.await.unwrap();
}

#[tokio::test]
async fn sessions_get_the_project_of_their_repo_once_it_is_discovered() {
    let tmp = tempfile::tempdir().unwrap();
    let app = tmp.path().join("app");
    git_repo(
        &app,
        "[remote \"origin\"]\n\turl = git@github.com:org/app.git\n",
    );
    let mut store = herder_store::Store::open(tmp.path().join("herder.db")).unwrap();
    store
        .append(herder_store::NewEvent {
            session_id: SessionId::new("s1"),
            at: jiff::Timestamp::now(),
            by: None,
            body: herder_protocol::EventBody::SessionCreated {
                repo: app.to_string_lossy().into_owned(),
                worktree: tmp.path().join("wt").to_string_lossy().into_owned(),
                branch: "b".into(),
                provider: herder_protocol::Provider::Claude,
                account_id: AccountId::new("a"),
                model: "m".into(),
                permission_mode: herder_protocol::PermissionMode::Ask,
                parent: None,
                task: None,
            },
        })
        .unwrap();
    let hub = Arc::new(Hub::default());
    let outbox = Arc::new(crate::hub::Outbox::default());
    hub.connect(&outbox, herder_protocol::Role::Member);
    let shutdown = CancellationToken::new();
    let sessions = SessionManager::open(
        crate::session::Setup {
            store,
            adapters: crate::session::Adapters::new(),
            accounts: crate::session::Accounts::new(),
            sink: Arc::clone(&hub) as Arc<dyn EventSink>,
            turn_ids: crate::session::ulid_turn_ids(),
            worktrees: crate::worktree::Worktrees::new(tmp.path().join("worktrees")),
        },
        shutdown.clone(),
    )
    .await
    .unwrap();
    assert_eq!(sessions.sessions().await.unwrap()[0].project_id, None);
    let task = tokio::spawn(
        Discovery {
            host: host(),
            config: ProjectsConfig::default(),
            hub: Arc::clone(&hub),
            sessions: sessions.clone(),
            sessions_changed: Arc::new(Notify::new()),
        }
        .run(shutdown.clone()),
    );

    let heads = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match outbox.pop() {
                Some(herder_protocol::ServerMessage::Sessions { sessions }) => return sessions,
                Some(_) => {}
                None => outbox.ready().await,
            }
        }
    })
    .await
    .expect("a session list");
    let app_id = Some(ProjectId::new("github.com/org/app"));
    assert_eq!(heads[0].project_id, app_id);
    assert_eq!(sessions.sessions().await.unwrap()[0].project_id, app_id);

    shutdown.cancel();
    task.await.unwrap();
}
