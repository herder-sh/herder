use std::process::Command;
use std::sync::Mutex;

use herder_protocol::Bytes;

use super::*;

/// Records everything published.
#[derive(Default)]
struct Sink {
    statuses: Mutex<Vec<SkillsStatus>>,
    sessions: Mutex<Vec<(SessionId, Vec<SessionSkill>)>>,
}

impl SkillsSink for Sink {
    fn skills_status(&self, status: SkillsStatus) {
        self.statuses.lock().unwrap().push(status);
    }

    fn session_skills(&self, session_id: &SessionId, skills: Vec<SessionSkill>) {
        self.sessions
            .lock()
            .unwrap()
            .push((session_id.clone(), skills));
    }
}

impl Sink {
    fn status(&self) -> SkillsStatus {
        self.statuses.lock().unwrap().last().unwrap().clone()
    }

    fn last_session(&self) -> (SessionId, Vec<SessionSkill>) {
        self.sessions.lock().unwrap().last().unwrap().clone()
    }
}

/// Runs git in `dir` as a test user, panicking when it fails; returns its stdout.
fn run_git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@localhost",
            "-c",
            "init.defaultBranch=main",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// A bare repository at `<root>/<name>.git`, with `files` committed when there are any.
fn bare_repo(root: &Path, name: &str, files: &[(&str, &str)]) -> PathBuf {
    let bare = root.join(format!("{name}.git"));
    run_git(root, &["init", "--quiet", "--bare", bare.to_str().unwrap()]);
    if !files.is_empty() {
        commit_to(&bare, root, files);
    }
    bare
}

/// Commits `files` to `bare` from a scratch clone, as another machine would.
fn commit_to(bare: &Path, root: &Path, files: &[(&str, &str)]) {
    let work = tempfile::tempdir_in(root).unwrap();
    run_git(
        root,
        &[
            "clone",
            "--quiet",
            bare.to_str().unwrap(),
            work.path().to_str().unwrap(),
        ],
    );
    for (path, text) in files {
        let path = work.path().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    run_git(work.path(), &["add", "--all"]);
    run_git(work.path(), &["commit", "--quiet", "-m", "fixture"]);
    run_git(work.path(), &["push", "--quiet", "origin", "HEAD"]);
}

fn skill_md(name: &str, description: &str) -> String {
    format!("---\nname: {name}\ndescription: {description}\n---\n\nDo it.\n")
}

fn file(path: &str, text: &str, executable: bool) -> SkillFile {
    SkillFile {
        path: path.into(),
        data: Bytes(text.as_bytes().to_vec()),
        executable,
    }
}

const ALL: [Provider; 5] = [
    Provider::Claude,
    Provider::Codex,
    Provider::Cursor,
    Provider::Opencode,
    Provider::Grok,
];

struct Fixture {
    root: tempfile::TempDir,
    data: PathBuf,
    sink: Arc<Sink>,
    skills: Skills,
}

fn fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    fs::create_dir(&data).unwrap();
    let sink = Arc::new(Sink::default());
    let skills = Skills::open(&data, &ALL, Arc::clone(&sink) as Arc<dyn SkillsSink>).unwrap();
    Fixture {
        root,
        data,
        sink,
        skills,
    }
}

impl Fixture {
    async fn run(&self, command: CommandBody) -> Result<CommandResult, ErrorInfo> {
        self.skills.command(command).await
    }

    async fn set_repo(&self, bare: &Path) {
        let url = bare.to_str().unwrap().to_owned();
        self.run(CommandBody::SetSkillsRepo { url }).await.unwrap();
    }

    fn names(&self) -> Vec<String> {
        self.sink
            .status()
            .skills
            .into_iter()
            .map(|skill| skill.name)
            .collect()
    }
}

/// The names in the link dir `dir` of the fixture's data dir.
fn linked(data: &Path, dir: &str) -> Vec<String> {
    let mut names: Vec<_> = fs::read_dir(data.join("skill-links").join(dir))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

#[tokio::test]
async fn clones_commits_pushes_and_pulls_the_library() {
    let f = fixture();
    let bare = bare_repo(
        f.root.path(),
        "library",
        &[("review/SKILL.md", &skill_md("review", "Review a diff"))],
    );
    f.set_repo(&bare).await;
    let status = f.sink.status();
    assert_eq!(status.repo.as_deref(), bare.to_str());
    assert_eq!(status.head, Some(run_git(&bare, &["rev-parse", "HEAD"])));
    assert!(status.last_pull.is_some());
    assert_eq!(status.pull_error, None);
    assert_eq!(
        status.skills,
        [LibrarySkill {
            name: "review".into(),
            description: "Review a diff".into(),
            enabled: true,
            providers: vec![
                Provider::Claude,
                Provider::Codex,
                Provider::Cursor,
                Provider::Opencode
            ],
        }]
    );
    assert_eq!(
        status.reload,
        [
            ProviderReload {
                provider: Provider::Claude,
                reload: SkillReload::Live
            },
            ProviderReload {
                provider: Provider::Codex,
                reload: SkillReload::NextTurn
            },
            ProviderReload {
                provider: Provider::Cursor,
                reload: SkillReload::NextSession
            },
            ProviderReload {
                provider: Provider::Opencode,
                reload: SkillReload::NextSession
            },
        ]
    );

    f.run(CommandBody::PutSkill {
        name: "deploy".into(),
        files: vec![
            file("SKILL.md", &skill_md("deploy", "Ship it"), false),
            file("scripts/run.sh", "#!/bin/sh\necho hi\n", true),
        ],
    })
    .await
    .unwrap();
    assert_eq!(
        run_git(&bare, &["show", "HEAD:deploy/SKILL.md"]),
        skill_md("deploy", "Ship it").trim()
    );
    let tree = run_git(&bare, &["ls-tree", "-r", "HEAD", "deploy/scripts/run.sh"]);
    assert!(tree.starts_with("100755"), "{tree}");
    assert_eq!(f.names(), ["deploy", "review"]);
    assert_eq!(
        f.sink.status().head,
        Some(run_git(&bare, &["rev-parse", "HEAD"]))
    );

    // Another machine adds a skill; a pull brings it here.
    commit_to(
        &bare,
        f.root.path(),
        &[("triage/SKILL.md", &skill_md("triage", "Sort issues"))],
    );
    f.run(CommandBody::PullSkills).await.unwrap();
    assert_eq!(f.names(), ["deploy", "review", "triage"]);
    assert_eq!(linked(&f.data, "enabled"), ["deploy", "review", "triage"]);

    f.run(CommandBody::DeleteSkill {
        name: "review".into(),
    })
    .await
    .unwrap();
    assert_eq!(
        run_git(&bare, &["ls-tree", "--name-only", "HEAD"]),
        "deploy\ntriage"
    );
    assert_eq!(f.names(), ["deploy", "triage"]);
    assert_eq!(
        linked(&f.data, "claude/.claude/skills"),
        ["deploy", "triage"]
    );
    let err = f
        .run(CommandBody::DeleteSkill {
            name: "review".into(),
        })
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);
}

#[tokio::test]
async fn the_first_skill_of_an_empty_library_is_pushed() {
    let f = fixture();
    let bare = bare_repo(f.root.path(), "empty", &[]);
    f.set_repo(&bare).await;
    assert_eq!(f.sink.status().head, None);
    f.run(CommandBody::PullSkills).await.unwrap();
    assert_eq!(f.sink.status().pull_error, None);
    f.run(CommandBody::PutSkill {
        name: "first".into(),
        files: vec![file("SKILL.md", &skill_md("first", "The first"), false)],
    })
    .await
    .unwrap();
    assert_eq!(
        f.sink.status().head,
        Some(run_git(&bare, &["rev-parse", "HEAD"]))
    );
    assert_eq!(f.names(), ["first"]);
}

#[tokio::test]
async fn a_change_that_cannot_be_pushed_is_undone() {
    let f = fixture();
    let bare = bare_repo(
        f.root.path(),
        "library",
        &[("review/SKILL.md", &skill_md("review", "Review a diff"))],
    );
    f.set_repo(&bare).await;
    let head = f.sink.status().head;
    let hook = bare.join("hooks/pre-receive");
    fs::write(&hook, "#!/bin/sh\necho refused >&2\nexit 1\n").unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    let err = f
        .run(CommandBody::PutSkill {
            name: "deploy".into(),
            files: vec![file("SKILL.md", &skill_md("deploy", "Ship it"), false)],
        })
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Internal);
    assert!(err.message.contains("cannot push"), "{}", err.message);
    assert!(!f.data.join("skills/deploy").exists());
    f.run(CommandBody::PullSkills).await.unwrap();
    assert_eq!(f.sink.status().head, head);
    assert_eq!(f.names(), ["review"]);
}

#[tokio::test]
async fn imports_a_skill_folder_from_another_repository() {
    let f = fixture();
    let library = bare_repo(
        f.root.path(),
        "library",
        &[("review/SKILL.md", &skill_md("review", "Review a diff"))],
    );
    let other = bare_repo(
        f.root.path(),
        "others",
        &[
            ("skills/pdf/SKILL.md", &skill_md("pdf", "Read PDFs")),
            ("skills/pdf/reference/forms.md", "# Forms\n"),
            ("skills/pdf/scripts/fill.py", "print('fill')\n"),
            ("README.md", "not a skill\n"),
        ],
    );
    f.set_repo(&library).await;
    f.run(CommandBody::ImportSkill {
        git_url: other.to_str().unwrap().into(),
        path: Some("skills/pdf".into()),
    })
    .await
    .unwrap();
    assert_eq!(
        run_git(&library, &["ls-tree", "-r", "--name-only", "HEAD", "pdf"]),
        "pdf/SKILL.md\npdf/reference/forms.md\npdf/scripts/fill.py"
    );
    assert_eq!(f.names(), ["pdf", "review"]);
    assert!(!f.data.join("skills.import").exists());

    let err = f
        .run(CommandBody::ImportSkill {
            git_url: other.to_str().unwrap().into(),
            path: None,
        })
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::BadRequest);
    let err = f
        .run(CommandBody::ImportSkill {
            git_url: other.to_str().unwrap().into(),
            path: Some("skills/../skills/pdf".into()),
        })
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::BadRequest);
}

#[tokio::test]
async fn imports_a_repository_that_is_one_skill_named_after_it() {
    let f = fixture();
    let library = bare_repo(f.root.path(), "library", &[]);
    let other = bare_repo(
        f.root.path(),
        "commit-style",
        &[("SKILL.md", &skill_md("commit-style", "Write commits"))],
    );
    f.set_repo(&library).await;
    f.run(CommandBody::ImportSkill {
        git_url: other.to_str().unwrap().into(),
        path: None,
    })
    .await
    .unwrap();
    assert_eq!(
        run_git(&library, &["ls-tree", "-r", "--name-only", "HEAD"]),
        "commit-style/SKILL.md"
    );
}

#[tokio::test]
async fn a_disabled_skill_is_left_out_on_this_machine() {
    let f = fixture();
    let bare = bare_repo(
        f.root.path(),
        "library",
        &[
            ("review/SKILL.md", &skill_md("review", "Review a diff")),
            ("deploy/SKILL.md", &skill_md("deploy", "Ship it")),
        ],
    );
    f.set_repo(&bare).await;
    let worktree = f.root.path().join("worktree");
    fs::create_dir(&worktree).unwrap();
    let session = SessionId::new("s1");
    f.skills
        .session_started(&session, &Provider::Claude, &AccountId::new("a"), &worktree)
        .await;
    f.run(CommandBody::SetSkillEnabled {
        name: "deploy".into(),
        enabled: false,
    })
    .await
    .unwrap();
    let deploy = f.sink.status().skills.remove(0);
    assert_eq!(deploy.name, "deploy");
    assert!(!deploy.enabled);
    assert!(deploy.providers.is_empty());
    for dir in ["enabled", "claude/.claude/skills", "cursor/skills"] {
        assert_eq!(linked(&f.data, dir), ["review"], "{dir}");
    }
    let (id, skills) = f.sink.last_session();
    assert_eq!(id, session);
    let names: Vec<_> = skills.iter().map(|skill| skill.name.as_str()).collect();
    assert_eq!(names, ["review"]);
    // The library itself is untouched.
    assert_eq!(
        run_git(&bare, &["ls-tree", "--name-only", "HEAD"]),
        "deploy\nreview"
    );

    // Kept on this machine across a restart.
    let sink = Arc::new(Sink::default());
    let reopened = Skills::open(&f.data, &ALL, Arc::clone(&sink) as Arc<dyn SkillsSink>).unwrap();
    reopened.pull().await;
    assert!(!sink.status().skills[0].enabled);
    reopened
        .command(CommandBody::SetSkillEnabled {
            name: "deploy".into(),
            enabled: true,
        })
        .await
        .unwrap();
    assert_eq!(linked(&f.data, "enabled"), ["deploy", "review"]);

    let err = f
        .run(CommandBody::SetSkillEnabled {
            name: "nope".into(),
            enabled: false,
        })
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);
}

#[tokio::test]
async fn each_cli_is_handed_its_layout_of_the_library() {
    let f = fixture();
    let links = f.data.join("skill-links");
    assert_eq!(
        f.skills.launch(&Provider::Claude, None),
        Some(links.join("claude"))
    );
    assert_eq!(
        f.skills.launch(&Provider::Cursor, None),
        Some(links.join("cursor"))
    );
    let plugin: serde_json::Value =
        serde_json::from_slice(&fs::read(links.join("cursor/.cursor-plugin/plugin.json")).unwrap())
            .unwrap();
    assert_eq!(plugin["name"], "herder");
    assert_eq!(
        f.skills.launch(&Provider::Opencode, None),
        Some(links.join("enabled"))
    );
    assert_eq!(f.skills.launch(&Provider::Grok, None), None);
    assert_eq!(f.skills.launch(&Provider::Gemini, None), None);

    // Codex gets one link in its config dir, and nothing else there changes.
    let codex_home = f.root.path().join("codex");
    fs::create_dir(&codex_home).unwrap();
    fs::write(codex_home.join("config.toml"), "model = \"x\"\n").unwrap();
    assert_eq!(f.skills.launch(&Provider::Codex, Some(&codex_home)), None);
    let link = codex_home.join("skills/herder");
    assert_eq!(fs::read_link(&link).unwrap(), links.join("enabled"));
    assert_eq!(f.skills.launch(&Provider::Codex, Some(&codex_home)), None);
    assert_eq!(fs::read_link(&link).unwrap(), links.join("enabled"));
    assert_eq!(
        fs::read_to_string(codex_home.join("config.toml")).unwrap(),
        "model = \"x\"\n"
    );
    // A link left pointing elsewhere is herder's to fix; a real dir is the user's.
    fs::remove_file(&link).unwrap();
    symlink("/elsewhere", &link).unwrap();
    f.skills.launch(&Provider::Codex, Some(&codex_home));
    assert_eq!(fs::read_link(&link).unwrap(), links.join("enabled"));
    fs::remove_file(&link).unwrap();
    fs::create_dir(&link).unwrap();
    f.skills.launch(&Provider::Codex, Some(&codex_home));
    assert!(fs::symlink_metadata(&link).unwrap().is_dir());

    // A machine without Claude hands it nothing.
    let sink: Arc<dyn SkillsSink> = Arc::new(Sink::default());
    let without = Skills::open(&f.data, &[Provider::Codex], sink).unwrap();
    assert_eq!(without.launch(&Provider::Claude, None), None);
}

#[tokio::test]
async fn project_skills_are_found_in_nested_dirs_where_each_cli_looks() {
    let f = fixture();
    let worktree = f.root.path().join("worktree");
    for (path, text) in [
        (".claude/skills/lint/SKILL.md", skill_md("lint", "Lint it")),
        (
            "trip1-frontend/.claude/skills/storybook/SKILL.md",
            skill_md("storybook", "Run Storybook"),
        ),
        (
            "web/.agents/skills/deploy/SKILL.md",
            skill_md("deploy", "Deploy the web app"),
        ),
        (
            "node_modules/pkg/.claude/skills/vendored/SKILL.md",
            skill_md("vendored", "Not the project's"),
        ),
        (".claude/skills/notes.md", "not a skill".to_owned()),
    ] {
        let path = worktree.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    let paths = |provider: Provider| -> Vec<(String, String)> {
        project_skills(&worktree, &provider)
            .into_iter()
            .map(|skill| {
                assert_eq!(skill.source, SkillSource::Project);
                (skill.name, skill.path.unwrap())
            })
            .collect()
    };
    let claude = [
        ("lint".to_owned(), ".claude/skills/lint".to_owned()),
        (
            "storybook".to_owned(),
            "trip1-frontend/.claude/skills/storybook".to_owned(),
        ),
    ];
    assert_eq!(paths(Provider::Claude), claude);
    assert_eq!(
        paths(Provider::Codex),
        [("deploy".to_owned(), "web/.agents/skills/deploy".to_owned())]
    );
    assert_eq!(paths(Provider::Opencode).len(), 3);

    // A session's skills: the library's and the project's, ordered by name.
    let bare = bare_repo(
        f.root.path(),
        "library",
        &[("review/SKILL.md", &skill_md("review", "Review a diff"))],
    );
    f.set_repo(&bare).await;
    let session = SessionId::new("s1");
    f.skills
        .session_started(&session, &Provider::Claude, &AccountId::new("a"), &worktree)
        .await;
    let (_, skills) = f.sink.last_session();
    assert_eq!(skills[0].description, "Lint it");
    assert_eq!(
        skills[1],
        SessionSkill {
            name: "review".into(),
            description: "Review a diff".into(),
            source: SkillSource::Library,
            path: None,
        }
    );
    assert_eq!(skills.len(), 3);
    // A library change sends them again; archive sends none.
    f.run(CommandBody::DeleteSkill {
        name: "review".into(),
    })
    .await
    .unwrap();
    assert_eq!(f.sink.last_session().1.len(), 2);
    f.skills.session_archived(&session);
    assert_eq!(f.sink.last_session(), (session, Vec::new()));
}

#[tokio::test]
async fn refuses_a_put_the_protocol_refuses() {
    let f = fixture();
    let put = |name: &str, files: Vec<SkillFile>| CommandBody::PutSkill {
        name: name.into(),
        files,
    };
    let md = || file("SKILL.md", &skill_md("x", "y"), false);
    let bare = bare_repo(f.root.path(), "library", &[]);
    f.set_repo(&bare).await;
    for files in [
        vec![file("README.md", "no SKILL.md", false)],
        vec![md(), file("../escape.sh", "", false)],
        vec![md(), file("a//b", "", false)],
        vec![md(), file(".git/config", "", false)],
        vec![md(), file("big", &"x".repeat(MAX_SKILL_BYTES), false)],
    ] {
        let err = f.run(put("ok", files)).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::BadRequest);
    }
    let err = f.run(put("Bad--Name", vec![md()])).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::BadRequest);
    let err = f
        .run(CommandBody::SetSkillsRepo {
            url: f.root.path().join("missing.git").to_str().unwrap().into(),
        })
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::BadRequest);
    // The library it had is kept.
    assert_eq!(f.sink.status().repo.as_deref(), bare.to_str());
}

#[tokio::test]
async fn a_library_without_a_repository_is_the_machines_own_until_one_is_set() {
    let f = fixture();
    f.run(CommandBody::PutSkill {
        name: "notes".into(),
        files: vec![file("SKILL.md", &skill_md("notes", "Take notes"), false)],
    })
    .await
    .unwrap();
    let status = f.sink.status();
    assert_eq!(status.repo, None);
    assert!(status.head.is_some(), "committed on the machine");
    assert_eq!(f.names(), ["notes"]);
    assert_eq!(linked(&f.data, "enabled"), ["notes"]);
    let err = f.run(CommandBody::PullSkills).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);

    // A put the protocol refuses leaves the machine's library as it was.
    let refused = f
        .run(CommandBody::PutSkill {
            name: "notes".into(),
            files: vec![file("README.md", "no SKILL.md", false)],
        })
        .await;
    assert!(refused.is_err());
    assert_eq!(f.names(), ["notes"]);

    // Setting a repository carries the machine's skills into it, beside its own.
    let bare = bare_repo(
        f.root.path(),
        "library",
        &[("review/SKILL.md", &skill_md("review", "Review a diff"))],
    );
    f.set_repo(&bare).await;
    assert_eq!(f.sink.status().repo.as_deref(), bare.to_str());
    assert_eq!(f.names(), ["notes", "review"]);
    let other = f.root.path().join("other");
    run_git(
        f.root.path(),
        &[
            "clone",
            "--quiet",
            bare.to_str().unwrap(),
            other.to_str().unwrap(),
        ],
    );
    assert!(
        other.join("notes/SKILL.md").is_file(),
        "pushed to the repository"
    );
}

#[tokio::test]
async fn an_accounts_own_skills_are_listed_and_reach_its_sessions() {
    let f = fixture();
    let config = f.root.path().join("claude-home");
    let write = |path: &str, text: &str| {
        let path = config.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    };
    write(
        "skills/plain/SKILL.md",
        "---\ndescription: No name of its own\n---\n",
    );
    write(
        "skills/synced/bucket/pdf/SKILL.md",
        &skill_md("pdf", "Read PDFs"),
    );
    write("skills/.trash/old/SKILL.md", &skill_md("old", "Deleted"));
    write("skills/empty/README.md", "no skill here");
    // Codex's config dir links herder's library in; it is listed as the library already.
    symlink(
        f.data.join("skill-links/enabled"),
        config.join("skills/herder"),
    )
    .unwrap();
    f.run(CommandBody::PutSkill {
        name: "notes".into(),
        files: vec![file("SKILL.md", &skill_md("notes", "Take notes"), false)],
    })
    .await
    .unwrap();

    let account = AccountId::new("work");
    f.skills.set_accounts(vec![
        (account.clone(), config.clone()),
        (AccountId::new("bare"), f.root.path().join("nothing")),
    ]);
    f.skills.pull().await;
    let accounts = f.sink.status().accounts;
    assert_eq!(accounts.len(), 1, "an account without skills is left out");
    assert_eq!(accounts[0].account_id, account);
    let listed: Vec<_> = accounts[0]
        .skills
        .iter()
        .map(|skill| (skill.name.as_str(), skill.path.as_deref(), skill.source))
        .collect();
    assert_eq!(
        listed,
        [
            (
                "pdf",
                Some("skills/synced/bucket/pdf"),
                SkillSource::Account
            ),
            ("plain", Some("skills/plain"), SkillSource::Account),
        ]
    );

    let worktree = f.root.path().join("worktree");
    fs::create_dir(&worktree).unwrap();
    f.skills
        .session_started(
            &SessionId::new("s1"),
            &Provider::Claude,
            &account,
            &worktree,
        )
        .await;
    let (_, skills) = f.sink.last_session();
    let names: Vec<_> = skills
        .iter()
        .map(|skill| (skill.name.as_str(), skill.source))
        .collect();
    assert_eq!(
        names,
        [
            ("notes", SkillSource::Library),
            ("pdf", SkillSource::Account),
            ("plain", SkillSource::Account),
        ]
    );
}

#[test]
fn reads_the_description_from_the_front_matter() {
    assert_eq!(description(&skill_md("a", "Plain words")), "Plain words");
    assert_eq!(
        description("---\nname: a\ndescription: \"Quoted: yes\"\n---\n"),
        "Quoted: yes"
    );
    assert_eq!(
        description("---\ndescription: >\n  Folded over\n  two lines\nname: a\n---\n"),
        "Folded over two lines"
    );
    assert_eq!(description("# No front matter\n"), "");
    assert_eq!(description("---\nname: a\n---\ndescription: body\n"), "");
}

#[test]
fn credentials_never_leave_a_url() {
    assert_eq!(
        redact("https://user:ghp_secret@github.com/you/skills.git"),
        "https://github.com/you/skills.git"
    );
    assert_eq!(
        redact("git@github.com:you/skills.git"),
        "git@github.com:you/skills.git"
    );
    assert_eq!(
        repo_name("https://github.com/you/commit-style.git"),
        "commit-style"
    );
    assert_eq!(repo_name("git@github.com:you/pdf"), "pdf");
}
