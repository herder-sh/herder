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
        default_permission_mode: None,
        default_account: None,
        setup_command: None,
        icon: None,
        icon_uploaded: false,
        icon_background: None,
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
            attachments: tmp.path().join("attachments"),
        },
        shutdown.clone(),
    )
    .await
    .unwrap();
    let sessions_changed = Arc::new(Notify::new());
    let task = tokio::spawn(
        Discovery {
            host: host(),
            config: Arc::new(Overrides::new(
                tmp.path().join("daemon.toml"),
                tmp.path().join("project-icons"),
                ProjectsConfig {
                    roots: vec![root.clone()],
                    entries: vec![ProjectEntry {
                        name: Some("Notes".to_owned()),
                        paths: vec![declared.clone()],
                        ..ProjectEntry::default()
                    }],
                    ..ProjectsConfig::default()
                },
            )),
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
                branch: Some("b".into()),
                provider: herder_protocol::Provider::Claude,
                account_id: AccountId::new("a"),
                model: "m".into(),
                permission_mode: herder_protocol::PermissionMode::Ask,
                failover_pin: None,
                parent: None,
                parent_host: None,
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
            attachments: tmp.path().join("attachments"),
        },
        shutdown.clone(),
    )
    .await
    .unwrap();
    assert_eq!(sessions.sessions().await.unwrap()[0].project_id, None);
    let task = tokio::spawn(
        Discovery {
            host: host(),
            config: Arc::new(Overrides::new(
                tmp.path().join("daemon.toml"),
                tmp.path().join("project-icons"),
                ProjectsConfig::default(),
            )),
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

#[tokio::test]
async fn owners_add_projects_and_set_their_settings_into_the_config_file() {
    let tmp = tempfile::tempdir().unwrap();
    let app = tmp.path().join("src/app");
    git_repo(
        &app,
        "[remote \"origin\"]\n\turl = git@github.com:org/app.git\n",
    );
    let file = tmp.path().join("daemon.toml");
    fs::write(
        &file,
        "# mine\n[[accounts]]\nid = \"main\"\nprovider = \"claude\"\n",
    )
    .unwrap();
    let hub = Arc::new(Hub::default());
    let outbox = Arc::new(crate::hub::Outbox::default());
    hub.connect(&outbox, herder_protocol::Role::Owner);
    let shutdown = CancellationToken::new();
    let sessions = SessionManager::open(
        crate::session::Setup {
            store: herder_store::Store::open(tmp.path().join("herder.db")).unwrap(),
            adapters: crate::session::Adapters::new(),
            accounts: crate::session::Accounts::from([(
                AccountId::new("main"),
                crate::session::AccountConfig {
                    provider: herder_protocol::Provider::Claude,
                    label: "Main".into(),
                    config_dir: None,
                },
            )]),
            sink: Arc::clone(&hub) as Arc<dyn EventSink>,
            turn_ids: crate::session::ulid_turn_ids(),
            worktrees: crate::worktree::Worktrees::new(tmp.path().join("worktrees")),
            attachments: tmp.path().join("attachments"),
        },
        shutdown.clone(),
    )
    .await
    .unwrap();
    let overrides = Arc::new(Overrides::new(
        file.clone(),
        tmp.path().join("project-icons"),
        ProjectsConfig::default(),
    ));
    sessions
        .manage_projects(host(), Arc::clone(&overrides))
        .unwrap();
    let task = tokio::spawn(
        Discovery {
            host: host(),
            config: overrides,
            hub: Arc::clone(&hub),
            sessions: sessions.clone(),
            sessions_changed: Arc::new(Notify::new()),
        }
        .run(shutdown.clone()),
    );
    assert_eq!(next_projects(&outbox).await, []);
    let owner = herder_protocol::UserId::new("owner");
    let handle = |command| sessions.handle(owner.clone(), command);

    let missing = herder_protocol::CommandBody::AddProject {
        path: tmp.path().join("nowhere").to_string_lossy().into_owned(),
    };
    let error = handle(missing).await.unwrap_err();
    assert_eq!(error.code, herder_protocol::ErrorCode::BadRequest);
    let add = herder_protocol::CommandBody::AddProject {
        path: format!("{}/", app.display()),
    };
    let added = herder_protocol::CommandResult::ProjectAdded {
        project_id: ProjectId::new("github.com/org/app"),
    };
    assert_eq!(handle(add.clone()).await, Ok(added.clone()));
    let path = app.to_string_lossy().into_owned();
    assert_eq!(
        next_projects(&outbox).await,
        [project("github.com/org/app", "app", &[&path])]
    );
    // Adding it again changes nothing.
    assert_eq!(handle(add).await, Ok(added));

    let set = |name: &str, default_account: &str, icon_background: &str| {
        herder_protocol::CommandBody::SetProjectSettings {
            project_id: ProjectId::new("github.com/org/app"),
            name: Some(name.into()),
            default_permission_mode: Some(herder_protocol::PermissionMode::AutoEdit),
            default_account: Some(AccountId::new(default_account)),
            setup_command: Some("make setup".into()),
            icon_background: Some(icon_background.into()),
        }
    };
    let error = handle(set("app", "nobody", "#ffffff")).await.unwrap_err();
    assert_eq!(error.code, herder_protocol::ErrorCode::NotFound);
    let error = handle(set("app", "main", "white")).await.unwrap_err();
    assert_eq!(error.code, herder_protocol::ErrorCode::BadRequest);
    assert_eq!(
        handle(set(" The app ", "main", "#ffffff")).await,
        Ok(herder_protocol::CommandResult::Applied)
    );
    let mut expected = project("github.com/org/app", "The app", &[&path]);
    expected.default_permission_mode = Some(herder_protocol::PermissionMode::AutoEdit);
    expected.default_account = Some(AccountId::new("main"));
    expected.setup_command = Some("make setup".into());
    expected.icon_background = Some("#ffffff".into());
    assert_eq!(next_projects(&outbox).await, [expected.clone()]);
    let named = crate::config::read_projects(&file).unwrap().entries;
    assert_eq!(named[0].name.as_deref(), Some("The app"));
    // The name it gets without one, or a blank one, clears the configured name.
    for name in ["app", " "] {
        handle(set("The app", "main", "#ffffff")).await.unwrap();
        handle(set(name, "main", "#ffffff")).await.unwrap();
        let entries = crate::config::read_projects(&file).unwrap().entries;
        assert_eq!(entries[0].name, None, "{name:?}");
    }
    expected.name = "app".into();
    let mut listed = next_projects(&outbox).await;
    while listed != [expected.clone()] {
        listed = next_projects(&outbox).await;
    }
    let unknown = herder_protocol::CommandBody::SetProjectSettings {
        project_id: ProjectId::new("github.com/org/other"),
        name: None,
        default_permission_mode: None,
        default_account: None,
        setup_command: None,
        icon_background: None,
    };
    let error = handle(unknown).await.unwrap_err();
    assert_eq!(error.code, herder_protocol::ErrorCode::NotFound);

    // A folder that is not a git repository is a project of this host.
    let notes = tmp.path().join("notes");
    fs::create_dir(&notes).unwrap();
    let notes_path = notes.to_string_lossy().into_owned();
    let local = ProjectId::local(&host(), &notes_path);
    let add = herder_protocol::CommandBody::AddProject {
        path: notes_path.clone(),
    };
    assert_eq!(
        handle(add).await,
        Ok(herder_protocol::CommandResult::ProjectAdded {
            project_id: local.clone()
        })
    );
    // Lists from the renames above may come first.
    while !next_projects(&outbox)
        .await
        .iter()
        .any(|project| project.project_id == local && project.paths == [notes_path.clone()])
    {}

    // The file keeps what was there and holds the project, as a restart reads it.
    let text = fs::read_to_string(&file).unwrap();
    assert!(text.starts_with("# mine\n"), "{text}");
    let entries = crate::config::read_projects(&file).unwrap().entries;
    assert_eq!(
        entries,
        [
            ProjectEntry {
                paths: vec![app.clone()],
                default_account: Some(AccountId::new("main")),
                default_permission_mode: Some(herder_protocol::PermissionMode::AutoEdit),
                setup_command: Some("make setup".into()),
                icon_background: Some("#ffffff".into()),
                ..ProjectEntry::default()
            },
            ProjectEntry {
                paths: vec![notes],
                ..ProjectEntry::default()
            }
        ]
    );

    shutdown.cancel();
    task.await.unwrap();
}

#[tokio::test]
async fn owners_remove_projects_without_live_sessions_and_keep_their_clones() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("Projects");
    let (app, lib) = (root.join("app"), root.join("lib"));
    git_repo(
        &app,
        "[remote \"origin\"]\n\turl = git@github.com:org/app.git\n",
    );
    git_repo(
        &lib,
        "[remote \"origin\"]\n\turl = git@github.com:org/lib.git\n",
    );
    let file = tmp.path().join("daemon.toml");
    fs::write(
        &file,
        format!(
            "# mine\n[projects]\nroots = [\"{}\"]\n\n[[project]]\nname = \"Lib\"\npaths = [\"{}\"]\n",
            root.display(),
            lib.display()
        ),
    )
    .unwrap();
    // A live session in app, and an archived one in lib.
    let mut store = herder_store::Store::open(tmp.path().join("herder.db")).unwrap();
    for (id, repo) in [("s1", &app), ("s2", &lib)] {
        store
            .append(herder_store::NewEvent {
                session_id: SessionId::new(id),
                at: jiff::Timestamp::now(),
                by: None,
                body: herder_protocol::EventBody::SessionCreated {
                    repo: repo.to_string_lossy().into_owned(),
                    worktree: tmp.path().join(id).to_string_lossy().into_owned(),
                    branch: Some("b".into()),
                    provider: herder_protocol::Provider::Claude,
                    account_id: AccountId::new("a"),
                    model: "m".into(),
                    permission_mode: herder_protocol::PermissionMode::Ask,
                    failover_pin: None,
                    parent: None,
                    parent_host: None,
                    task: None,
                },
            })
            .unwrap();
    }
    store
        .append(herder_store::NewEvent {
            session_id: SessionId::new("s2"),
            at: jiff::Timestamp::now(),
            by: None,
            body: herder_protocol::EventBody::SessionStatusChanged {
                retry_at: None,
                status: herder_protocol::SessionStatus::Archived,
            },
        })
        .unwrap();
    let hub = Arc::new(Hub::default());
    let outbox = Arc::new(crate::hub::Outbox::default());
    hub.connect(&outbox, herder_protocol::Role::Owner);
    let shutdown = CancellationToken::new();
    let sessions = SessionManager::open(
        crate::session::Setup {
            store,
            adapters: crate::session::Adapters::new(),
            accounts: crate::session::Accounts::new(),
            sink: Arc::clone(&hub) as Arc<dyn EventSink>,
            turn_ids: crate::session::ulid_turn_ids(),
            worktrees: crate::worktree::Worktrees::new(tmp.path().join("worktrees")),
            attachments: tmp.path().join("attachments"),
        },
        shutdown.clone(),
    )
    .await
    .unwrap();
    let overrides = Arc::new(Overrides::new(
        file.clone(),
        tmp.path().join("project-icons"),
        crate::config::read_projects(&file).unwrap(),
    ));
    sessions
        .manage_projects(host(), Arc::clone(&overrides))
        .unwrap();
    let task = tokio::spawn(
        Discovery {
            host: host(),
            config: overrides,
            hub: Arc::clone(&hub),
            sessions: sessions.clone(),
            sessions_changed: Arc::new(Notify::new()),
        }
        .run(shutdown.clone()),
    );
    let (app_path, lib_path) = (
        app.to_string_lossy().into_owned(),
        lib.to_string_lossy().into_owned(),
    );
    let both = [
        project("github.com/org/app", "app", &[&app_path]),
        project("github.com/org/lib", "Lib", &[&lib_path]),
    ];
    assert_eq!(next_projects(&outbox).await, both);
    let owner = herder_protocol::UserId::new("owner");
    let handle = |command| sessions.handle(owner.clone(), command);
    let remove = |id: &str| herder_protocol::CommandBody::RemoveProject {
        project_id: ProjectId::new(id),
    };

    let error = handle(remove("github.com/org/app")).await.unwrap_err();
    assert_eq!(error.code, herder_protocol::ErrorCode::Conflict);
    assert!(
        error.message.contains("1 live session"),
        "{}",
        error.message
    );
    let error = handle(remove("github.com/org/other")).await.unwrap_err();
    assert_eq!(error.code, herder_protocol::ErrorCode::NotFound);

    // Removed, it stays out though the roots scan and its archived session find it.
    assert_eq!(
        handle(remove("github.com/org/lib")).await,
        Ok(herder_protocol::CommandResult::Applied)
    );
    assert_eq!(next_projects(&outbox).await, both[..1]);
    assert!(lib.join(".git/config").is_file());
    let config = crate::config::read_projects(&file).unwrap();
    assert_eq!(
        (config.entries, config.exclude),
        (vec![], vec![lib.clone()])
    );
    assert!(
        fs::read_to_string(&file).unwrap().starts_with("# mine\n"),
        "the rest of the file is kept"
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let heads = sessions.sessions().await.unwrap();
            if heads
                .iter()
                .all(|head| head.project_id.is_some() == (head.session_id == SessionId::new("s1")))
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the archived session loses its project");

    // Added again, it is back.
    let add = herder_protocol::CommandBody::AddProject { path: lib_path };
    assert_eq!(
        handle(add).await,
        Ok(herder_protocol::CommandResult::ProjectAdded {
            project_id: ProjectId::new("github.com/org/lib"),
        })
    );
    assert_eq!(
        next_projects(&outbox).await,
        [
            both[0].clone(),
            project("github.com/org/lib", "lib", &[&both[1].paths[0]])
        ]
    );
    assert!(
        crate::config::read_projects(&file)
            .unwrap()
            .exclude
            .is_empty()
    );

    shutdown.cancel();
    task.await.unwrap();
}

#[tokio::test]
async fn discovery_lists_icons_and_anyone_fetches_them_afresh() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("Projects");
    let (app, lib) = (root.join("app"), root.join("lib"));
    git_repo(
        &app,
        "[remote \"origin\"]\n\turl = git@github.com:org/app.git\n",
    );
    git_repo(
        &lib,
        "[remote \"origin\"]\n\turl = git@github.com:org/lib.git\n",
    );
    fs::create_dir_all(app.join("public")).unwrap();
    fs::write(app.join("public/favicon.svg"), b"<svg/>").unwrap();
    fs::write(app.join("logo.png"), b"png").unwrap();
    let file = tmp.path().join("daemon.toml");
    fs::write(
        &file,
        format!(
            "[projects]\nroots = [\"{}\"]\n\n[[project]]\nremotes = [\"git@github.com:org/app.git\"]\nicon = \"logo.png\"\n",
            root.display(),
        ),
    )
    .unwrap();
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
            attachments: tmp.path().join("attachments"),
        },
        shutdown.clone(),
    )
    .await
    .unwrap();
    let overrides = Arc::new(Overrides::new(
        file.clone(),
        tmp.path().join("project-icons"),
        crate::config::read_projects(&file).unwrap(),
    ));
    sessions
        .manage_projects(host(), Arc::clone(&overrides))
        .unwrap();
    let task = tokio::spawn(
        Discovery {
            host: host(),
            config: overrides,
            hub: Arc::clone(&hub),
            sessions: sessions.clone(),
            sessions_changed: Arc::new(Notify::new()),
        }
        .run(shutdown.clone()),
    );
    let sha = |data: &[u8]| -> String {
        use sha2::Digest;
        sha2::Sha256::digest(data)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    };

    // The entry's `icon` wins over the candidates; lib has none.
    let projects = next_projects(&outbox).await;
    let icons: Vec<_> = projects.iter().map(|p| p.icon.clone()).collect();
    assert_eq!(icons, [Some(sha(b"png")), None]);

    let member = herder_protocol::UserId::new("bob");
    let fetch = |id: &str| {
        sessions.handle(
            member.clone(),
            herder_protocol::CommandBody::GetProjectIcon {
                project_id: ProjectId::new(id),
            },
        )
    };
    assert_eq!(
        fetch("github.com/org/app").await,
        Ok(herder_protocol::CommandResult::ProjectIcon {
            icon: sha(b"png"),
            media_type: "image/png".into(),
            data: herder_protocol::Bytes(b"png".to_vec()),
        })
    );
    let error = fetch("github.com/org/lib").await.unwrap_err();
    assert_eq!(
        (error.code, error.message.as_str()),
        (
            herder_protocol::ErrorCode::NotFound,
            "project github.com/org/lib has no icon"
        )
    );
    let error = fetch("github.com/org/other").await.unwrap_err();
    assert_eq!(error.code, herder_protocol::ErrorCode::NotFound);

    // Changed on disk, the icon is read afresh, and the next full scan lists its new hash.
    fs::remove_file(app.join("logo.png")).unwrap();
    let Ok(herder_protocol::CommandResult::ProjectIcon {
        icon, media_type, ..
    }) = fetch("github.com/org/app").await
    else {
        panic!("expected the favicon");
    };
    assert_eq!(
        (icon, media_type.as_str()),
        (sha(b"<svg/>"), "image/svg+xml")
    );
    let set = herder_protocol::CommandBody::SetProjectSettings {
        project_id: ProjectId::new("github.com/org/lib"),
        name: None,
        default_permission_mode: None,
        default_account: None,
        setup_command: Some("true".into()),
        icon_background: None,
    };
    assert_eq!(
        sessions.handle(member.clone(), set).await,
        Ok(herder_protocol::CommandResult::Applied)
    );
    let projects = next_projects(&outbox).await;
    assert_eq!(projects[0].icon, Some(sha(b"<svg/>")));

    shutdown.cancel();
    task.await.unwrap();
}

#[tokio::test]
async fn an_uploaded_icon_wins_until_it_is_cleared() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("Projects");
    let app = root.join("app");
    git_repo(
        &app,
        "[remote \"origin\"]\n\turl = git@github.com:org/app.git\n",
    );
    fs::write(app.join("logo.png"), b"png").unwrap();
    let file = tmp.path().join("daemon.toml");
    fs::write(
        &file,
        format!(
            "[projects]\nroots = [\"{}\"]\n\n[[project]]\nremotes = [\"git@github.com:org/app.git\"]\nicon = \"logo.png\"\n",
            root.display(),
        ),
    )
    .unwrap();
    let hub = Arc::new(Hub::default());
    let outbox = Arc::new(crate::hub::Outbox::default());
    hub.connect(&outbox, herder_protocol::Role::Owner);
    let shutdown = CancellationToken::new();
    let sessions = SessionManager::open(
        crate::session::Setup {
            store: herder_store::Store::open(tmp.path().join("herder.db")).unwrap(),
            adapters: crate::session::Adapters::new(),
            accounts: crate::session::Accounts::new(),
            sink: Arc::clone(&hub) as Arc<dyn EventSink>,
            turn_ids: crate::session::ulid_turn_ids(),
            worktrees: crate::worktree::Worktrees::new(tmp.path().join("worktrees")),
            attachments: tmp.path().join("attachments"),
        },
        shutdown.clone(),
    )
    .await
    .unwrap();
    let icons = tmp.path().join("project-icons");
    let overrides = Arc::new(Overrides::new(
        file.clone(),
        icons.clone(),
        crate::config::read_projects(&file).unwrap(),
    ));
    sessions
        .manage_projects(host(), Arc::clone(&overrides))
        .unwrap();
    let task = tokio::spawn(
        Discovery {
            host: host(),
            config: overrides,
            hub: Arc::clone(&hub),
            sessions: sessions.clone(),
            sessions_changed: Arc::new(Notify::new()),
        }
        .run(shutdown.clone()),
    );
    let sha = |data: &[u8]| -> String {
        use sha2::Digest;
        sha2::Sha256::digest(data)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    };
    let listed = |projects: Vec<Project>| (projects[0].icon.clone(), projects[0].icon_uploaded);
    assert_eq!(
        listed(next_projects(&outbox).await),
        (Some(sha(b"png")), false)
    );

    let owner = herder_protocol::UserId::new("owner");
    let handle = |command| sessions.handle(owner.clone(), command);
    let set = |media_type: &str, data: Vec<u8>| herder_protocol::CommandBody::SetProjectIcon {
        project_id: ProjectId::new("github.com/org/app"),
        icon: Some(herder_protocol::Image {
            media_type: media_type.into(),
            data: herder_protocol::Bytes(data),
        }),
    };
    let fetch = || {
        handle(herder_protocol::CommandBody::GetProjectIcon {
            project_id: ProjectId::new("github.com/org/app"),
        })
    };

    // Refused uploads change nothing.
    for bad in [
        set("image/gif", b"gif".to_vec()),
        set("image/png", Vec::new()),
        set(
            "image/png",
            vec![0; herder_protocol::MAX_PROJECT_ICON_BYTES + 1],
        ),
    ] {
        let error = handle(bad).await.unwrap_err();
        assert_eq!(error.code, herder_protocol::ErrorCode::BadRequest);
    }
    let unknown = herder_protocol::CommandBody::SetProjectIcon {
        project_id: ProjectId::new("github.com/org/other"),
        icon: None,
    };
    let error = handle(unknown).await.unwrap_err();
    assert_eq!(error.code, herder_protocol::ErrorCode::NotFound);
    assert!(!icons.exists());

    // An upload wins over the entry's icon and is listed at once.
    assert_eq!(
        handle(set("image/png", b"uploaded".to_vec())).await,
        Ok(herder_protocol::CommandResult::Applied)
    );
    assert_eq!(
        listed(next_projects(&outbox).await),
        (Some(sha(b"uploaded")), true)
    );
    assert_eq!(
        fetch().await,
        Ok(herder_protocol::CommandResult::ProjectIcon {
            icon: sha(b"uploaded"),
            media_type: "image/png".into(),
            data: herder_protocol::Bytes(b"uploaded".to_vec()),
        })
    );

    // A new upload of another type replaces it, and gives a new hash.
    assert_eq!(
        handle(set("image/svg+xml", b"<svg/>".to_vec())).await,
        Ok(herder_protocol::CommandResult::Applied)
    );
    assert_eq!(
        listed(next_projects(&outbox).await),
        (Some(sha(b"<svg/>")), true)
    );
    assert_eq!(fs::read_dir(&icons).unwrap().count(), 1);

    // Cleared, the upload's file is gone and the entry's icon is back.
    let clear = herder_protocol::CommandBody::SetProjectIcon {
        project_id: ProjectId::new("github.com/org/app"),
        icon: None,
    };
    assert_eq!(
        handle(clear).await,
        Ok(herder_protocol::CommandResult::Applied)
    );
    assert_eq!(
        listed(next_projects(&outbox).await),
        (Some(sha(b"png")), false)
    );
    assert_eq!(fs::read_dir(&icons).unwrap().count(), 0);
    let Ok(herder_protocol::CommandResult::ProjectIcon { icon, .. }) = fetch().await else {
        panic!("expected the entry's icon");
    };
    assert_eq!(icon, sha(b"png"));

    shutdown.cancel();
    task.await.unwrap();
}
