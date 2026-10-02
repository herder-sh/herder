use std::process::Command;

use super::*;

fn sh(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn write(dir: &Path, path: &str, contents: &str) {
    let path = dir.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

struct Repo {
    tmp: tempfile::TempDir,
    config: Config,
}

impl Repo {
    /// A repository with one commit of `README` and a `.gitignore` ignoring `ignored.txt`.
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        sh(&repo, &["init", "--quiet", "--initial-branch=main"]);
        sh(&repo, &["config", "user.name", "Someone"]);
        sh(&repo, &["config", "user.email", "someone@example.com"]);
        write(&repo, "README", "hello\n");
        write(&repo, ".gitignore", "ignored.txt\n");
        sh(&repo, &["add", "."]);
        sh(&repo, &["commit", "--quiet", "-m", "first"]);
        let config = Config {
            dir: tmp.path().join("checkpoints"),
            keep: KEEP,
            push_timeout: Duration::from_secs(30),
        };
        Self { tmp, config }
    }

    fn path(&self) -> PathBuf {
        self.tmp.path().join("repo")
    }

    /// A bare repository set as `origin`.
    fn origin(&self) -> PathBuf {
        let bare = self.tmp.path().join("origin.git");
        sh(
            self.tmp.path(),
            &["init", "--quiet", "--bare", bare.to_str().unwrap()],
        );
        sh(
            &self.path(),
            &["remote", "add", "origin", bare.to_str().unwrap()],
        );
        bare
    }

    async fn snapshot(&self, turn: &str) -> String {
        snapshot(&self.config, &self.path(), &session(), &TurnId::new(turn))
            .await
            .unwrap()
    }

    async fn publish(&self, turn: &str) -> Published {
        publish(&self.config, &self.path(), &session(), &TurnId::new(turn))
            .await
            .unwrap()
    }

    fn files(&self, rev: &str) -> Vec<String> {
        sh(&self.path(), &["ls-tree", "-r", "--name-only", rev])
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

fn session() -> SessionId {
    SessionId::new("01SESSION")
}

#[tokio::test]
async fn untracked_files_are_captured_and_secrets_never_are() {
    let repo = Repo::new();
    let path = repo.path();
    // A tracked secret is left out too.
    write(&path, "deploy/id_rsa", "key\n");
    sh(&path, &["add", "deploy/id_rsa"]);
    sh(&path, &["commit", "--quiet", "-m", "oops"]);
    write(&path, "README", "changed\n");
    write(&path, "new.txt", "new\n");
    write(&path, "src/lib.rs", "fn main() {}\n");
    write(&path, "ignored.txt", "ignored\n");
    for secret in [
        ".env",
        ".env.local",
        "app/.env",
        "app/.env.production",
        "certs/server.pem",
        "certs/server.key",
        "id_rsa.pub",
        ".npmrc",
        "py/.pypirc",
        "gcp/credentials.json",
        "store.p12",
    ] {
        write(&path, secret, "secret\n");
    }

    let name = repo.snapshot("01TURNA").await;

    assert_eq!(name, "refs/herder/01SESSION/01TURNA");
    assert_eq!(
        repo.files(&name),
        [".gitignore", "README", "new.txt", "src/lib.rs"]
    );
    assert_eq!(sh(&path, &["show", &format!("{name}:README")]), "changed");
    assert_eq!(
        sh(&path, &["rev-parse", &format!("{name}^")]),
        sh(&path, &["rev-parse", "HEAD"])
    );
    assert_eq!(
        sh(&path, &["log", "-1", "--format=%an <%ae>", &name]),
        "herder <herder@localhost>"
    );
}

#[tokio::test]
async fn the_users_index_head_and_branches_are_untouched() {
    let repo = Repo::new();
    let path = repo.path();
    write(&path, "README", "changed\n");
    write(&path, "staged.txt", "staged\n");
    sh(&path, &["add", "staged.txt"]);
    write(&path, "untracked.txt", "untracked\n");
    let index = std::fs::read(path.join(".git/index")).unwrap();
    let status = sh(&path, &["status", "--porcelain"]);
    let head = sh(&path, &["rev-parse", "HEAD"]);
    let refs = sh(&path, &["for-each-ref", "refs/heads", "refs/tags"]);

    repo.snapshot("01TURNA").await;

    assert_eq!(std::fs::read(path.join(".git/index")).unwrap(), index);
    assert_eq!(sh(&path, &["status", "--porcelain"]), status);
    assert_eq!(sh(&path, &["rev-parse", "HEAD"]), head);
    assert_eq!(sh(&path, &["symbolic-ref", "HEAD"]), "refs/heads/main");
    assert_eq!(
        sh(&path, &["for-each-ref", "refs/heads", "refs/tags"]),
        refs
    );
    // The temporary index is gone.
    let left: Vec<_> = std::fs::read_dir(repo.config.dir.join("01SESSION"))
        .unwrap()
        .collect();
    assert!(left.is_empty());
}

#[tokio::test]
async fn only_the_latest_checkpoints_are_kept() {
    let mut repo = Repo::new();
    repo.config.keep = 2;
    for turn in ["01TURNA", "01TURNB", "01TURNC"] {
        write(&repo.path(), "turn.txt", turn);
        repo.snapshot(turn).await;
    }
    assert_eq!(
        sh(
            &repo.path(),
            &["for-each-ref", "--format=%(refname)", "refs/herder/"]
        ),
        "refs/herder/01SESSION/01TURNB\nrefs/herder/01SESSION/01TURNC"
    );
}

#[tokio::test]
async fn without_a_remote_the_checkpoint_is_bundled() {
    let mut repo = Repo::new();
    repo.config.keep = 2;
    let mut bundles = Vec::new();
    for turn in ["01TURNA", "01TURNB", "01TURNC"] {
        write(&repo.path(), "turn.txt", turn);
        repo.snapshot(turn).await;
        let Published::Bundled(bundle) = repo.publish(turn).await else {
            panic!("pushed without a remote");
        };
        bundles.push(bundle);
    }

    let dir = repo.config.dir.join("01SESSION");
    assert_eq!(bundles[2], dir.join("01TURNC.bundle"));
    assert!(!bundles[0].exists());
    assert!(bundles[1].exists());
    let heads = sh(
        &repo.path(),
        &["bundle", "list-heads", bundles[2].to_str().unwrap()],
    );
    assert!(heads.ends_with(" refs/herder/01SESSION/01TURNC"), "{heads}");
    // The bundle restores the checkpoint into an empty repository.
    let restored = repo.tmp.path().join("restored");
    sh(
        repo.tmp.path(),
        &["init", "--quiet", "--bare", restored.to_str().unwrap()],
    );
    sh(
        &restored,
        &[
            "fetch",
            "--quiet",
            bundles[2].to_str().unwrap(),
            "refs/herder/*:refs/herder/*",
        ],
    );
    assert_eq!(
        sh(
            &restored,
            &["show", "refs/herder/01SESSION/01TURNC:turn.txt"]
        ),
        "01TURNC"
    );
}

#[tokio::test]
async fn a_failed_push_falls_back_to_a_bundle() {
    let repo = Repo::new();
    let missing = repo.tmp.path().join("missing.git");
    sh(
        &repo.path(),
        &["remote", "add", "origin", missing.to_str().unwrap()],
    );
    repo.snapshot("01TURNA").await;
    assert_eq!(
        repo.publish("01TURNA").await,
        Published::Bundled(repo.config.dir.join("01SESSION/01TURNA.bundle"))
    );
}

#[tokio::test]
async fn checkpoints_are_pushed_and_pruned_on_the_remote_without_branches_or_hooks() {
    let mut repo = Repo::new();
    repo.config.keep = 2;
    let path = repo.path();
    let bare = repo.origin();
    // Hooks that would leave a mark if git ran them.
    let hooks = repo.tmp.path().join("hooks");
    let marks = repo.tmp.path().join("marks");
    std::fs::create_dir(&marks).unwrap();
    for hook in [
        "pre-push",
        "reference-transaction",
        "post-index-change",
        "pre-commit",
        "commit-msg",
    ] {
        write(
            &hooks,
            hook,
            &format!("#!/bin/sh\ntouch {}/{hook}\n", marks.display()),
        );
        std::fs::set_permissions(
            hooks.join(hook),
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();
    }
    sh(
        &path,
        &["config", "core.hooksPath", hooks.to_str().unwrap()],
    );

    for turn in ["01TURNA", "01TURNB", "01TURNC"] {
        write(&path, "turn.txt", turn);
        repo.snapshot(turn).await;
        assert_eq!(repo.publish(turn).await, Published::Pushed);
    }

    assert_eq!(
        sh(&bare, &["for-each-ref", "--format=%(refname)"]),
        "refs/herder/01SESSION/01TURNB\nrefs/herder/01SESSION/01TURNC"
    );
    assert_eq!(
        sh(&bare, &["show", "refs/herder/01SESSION/01TURNC:turn.txt"]),
        "01TURNC"
    );
    let ran: Vec<_> = std::fs::read_dir(&marks).unwrap().collect();
    assert!(ran.is_empty(), "hooks ran: {ran:?}");
    assert!(!repo.config.dir.join("01SESSION/01TURNC.bundle").exists());
}
