//! Session worktrees on throwaway repositories. A separate test binary: the git processes
//! these spawn would otherwise briefly inherit the data-dir lock of `data_dir`'s tests.

use std::path::{Path, PathBuf};
use std::process::Command;

use herder_daemon::worktree::{Error, Worktrees, branches, slug};
use herder_protocol::SessionId;

/// Runs git in `dir` with a fixed identity, panicking on failure; returns trimmed stdout.
fn run(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=test", "-c", "user.email=test@example.com"])
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

/// A repository `<tmp>/app` with one commit on `main`, and its worktrees dir.
fn setup() -> (tempfile::TempDir, PathBuf, Worktrees) {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("app");
    std::fs::create_dir(&repo).unwrap();
    run(&repo, &["init", "--quiet", "--initial-branch=main"]);
    run(&repo, &["commit", "--quiet", "--allow-empty", "-m", "init"]);
    let worktrees = Worktrees::new(tmp.path().join("data/worktrees"));
    (tmp, repo, worktrees)
}

fn branch_exists(repo: &Path, name: &str) -> bool {
    !run(repo, &["branch", "--list", name]).is_empty()
}

#[test]
fn slug_is_the_lowercased_random_tail_of_the_id() {
    let id = SessionId::new("01K6HX3V5E8ZQW2M4N7P9RTB3C");
    assert_eq!(slug(&id), "7p9rtb3c");
    assert_eq!(slug(&SessionId::new("AB")), "ab");
}

#[tokio::test]
async fn create_adds_a_worktree_on_a_new_session_branch() {
    let (tmp, repo, worktrees) = setup();
    let worktree = worktrees.create(&repo, "ab12cd34", None).await.unwrap();
    assert_eq!(
        worktree.path,
        tmp.path().join("data/worktrees/app-ab12cd34")
    );
    assert_eq!(worktree.branch.as_deref(), Some("herder/ab12cd34"));
    assert!(worktree.path.join(".git").is_file());
    assert_eq!(
        run(&worktree.path, &["branch", "--show-current"]),
        "herder/ab12cd34"
    );
    assert_eq!(
        run(&worktree.path, &["rev-parse", "HEAD"]),
        run(&repo, &["rev-parse", "main"])
    );
    // The session branch does not track the default branch.
    let upstream = std::process::Command::new("git")
        .arg("-C")
        .arg(&worktree.path)
        .args(["rev-parse", "--abbrev-ref", "@{upstream}"])
        .output()
        .unwrap();
    assert!(!upstream.status.success());
}

#[tokio::test]
async fn create_uses_the_branch_the_command_names() {
    let (_tmp, repo, worktrees) = setup();
    let worktree = worktrees
        .create(&repo, "ab12cd34", Some("fix/login".into()))
        .await
        .unwrap();
    assert_eq!(worktree.branch.as_deref(), Some("fix/login"));
    assert!(worktree.path.ends_with("app-ab12cd34"));
    assert_eq!(
        run(&worktree.path, &["branch", "--show-current"]),
        "fix/login"
    );
}

#[tokio::test]
async fn create_rejects_bad_repos_and_branches() {
    let (_tmp, repo, worktrees) = setup();
    let err = worktrees
        .create(Path::new("relative"), "s1", None)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::BadRequest(_)), "{err:?}");
    let err = worktrees
        .create(&repo, "s1", Some("bad..name".into()))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::BadRequest(_)), "{err:?}");
    let err = worktrees
        .create(&repo, "s1", Some("main".into()))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Conflict(_)), "{err:?}");
}

#[tokio::test]
async fn a_folder_without_a_commit_is_worked_in_itself() {
    let (tmp, _repo, worktrees) = setup();
    let plain = tmp.path().join("plain");
    std::fs::create_dir(&plain).unwrap();
    let empty = tmp.path().join("empty");
    std::fs::create_dir(&empty).unwrap();
    run(&empty, &["init", "--quiet"]);
    for folder in [&plain, &empty] {
        let worktree = worktrees.create(folder, "s1", None).await.unwrap();
        assert_eq!(
            worktree,
            herder_daemon::worktree::Worktree {
                path: folder.clone(),
                branch: None
            }
        );
        // No branch to name, none to list.
        let err = worktrees
            .create(folder, "s1", Some("fix/login".into()))
            .await
            .unwrap_err();
        assert!(matches!(err, Error::BadRequest(_)), "{err:?}");
        assert!(branches(folder, None).await.unwrap().is_empty());
        // Removing leaves the folder alone.
        worktrees.remove(folder, folder).await.unwrap();
        assert!(folder.is_dir());
    }
    assert!(!tmp.path().join("data/worktrees").exists());
    assert_eq!(run(&empty, &["status", "--porcelain"]), "");
}

#[tokio::test]
async fn base_is_the_default_branch_not_whatever_is_checked_out() {
    let (_tmp, repo, worktrees) = setup();
    let main = run(&repo, &["rev-parse", "main"]);
    run(&repo, &["checkout", "--quiet", "-b", "dev"]);
    run(&repo, &["commit", "--quiet", "--allow-empty", "-m", "dev"]);
    let dev = run(&repo, &["rev-parse", "dev"]);

    // No origin: the repository's HEAD.
    let first = worktrees.create(&repo, "s1", None).await.unwrap();
    assert_eq!(run(&first.path, &["rev-parse", "HEAD"]), dev);

    // origin/HEAD names main: the local main, even though dev is checked out.
    run(&repo, &["update-ref", "refs/remotes/origin/main", &main]);
    run(
        &repo,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
    run(
        &repo,
        &["commit", "--quiet", "--allow-empty", "-m", "dev 2"],
    );
    run(&repo, &["checkout", "--quiet", "main"]);
    run(
        &repo,
        &["commit", "--quiet", "--allow-empty", "-m", "main 2"],
    );
    run(&repo, &["checkout", "--quiet", "dev"]);
    let second = worktrees.create(&repo, "s2", None).await.unwrap();
    assert_eq!(
        run(&second.path, &["rev-parse", "HEAD"]),
        run(&repo, &["rev-parse", "main"])
    );
}

#[tokio::test]
async fn branches_records_every_branch_checked_out_in_the_worktree() {
    let (_tmp, repo, worktrees) = setup();
    run(&repo, &["tag", "v1"]);
    run(&repo, &["branch", "existing"]);
    let worktree = worktrees.create(&repo, "s1", None).await.unwrap();
    let path = &worktree.path;
    assert_eq!(
        branches(path, worktree.branch.as_deref()).await.unwrap(),
        ["herder/s1"]
    );

    run(path, &["checkout", "--quiet", "-b", "feature"]);
    run(path, &["checkout", "--quiet", "v1"]);
    run(path, &["switch", "--quiet", "existing"]);
    run(path, &["checkout", "--quiet", "-b", "gone"]);
    run(path, &["checkout", "--quiet", "feature"]);
    run(path, &["branch", "--quiet", "-D", "gone"]);
    run(path, &["branch", "-m", "feature-renamed"]);
    run(path, &["checkout", "--quiet", "herder/s1"]);
    assert_eq!(
        branches(path, worktree.branch.as_deref()).await.unwrap(),
        [
            "herder/s1",
            "feature",
            "existing",
            "gone",
            "feature-renamed"
        ]
    );
    // Checkouts in the main worktree are not the session's.
    run(&repo, &["checkout", "--quiet", "-b", "elsewhere"]);
    assert!(
        !branches(path, worktree.branch.as_deref())
            .await
            .unwrap()
            .contains(&"elsewhere".to_owned())
    );
}

#[tokio::test]
async fn remove_takes_a_dirty_worktree_and_keeps_the_branches() {
    let (_tmp, repo, worktrees) = setup();
    let worktree = worktrees.create(&repo, "s1", None).await.unwrap();
    let path = &worktree.path;
    run(path, &["checkout", "--quiet", "-b", "side"]);
    std::fs::write(path.join("notes.txt"), "draft").unwrap();

    worktrees.remove(&repo, path).await.unwrap();
    assert!(!path.exists());
    assert_eq!(run(&repo, &["worktree", "list"]).lines().count(), 1);
    assert!(branch_exists(&repo, "herder/s1"));
    assert!(branch_exists(&repo, "side"));
}

#[tokio::test]
async fn remove_deletes_a_worktree_git_no_longer_knows() {
    let (_tmp, repo, worktrees) = setup();
    let worktree = worktrees.create(&repo, "s1", None).await.unwrap();
    let path = &worktree.path;
    // As a worktree left behind with only build output, its `.git` link gone.
    std::fs::remove_file(path.join(".git")).unwrap();
    std::fs::create_dir_all(path.join("apple/build")).unwrap();

    worktrees.remove(&repo, path).await.unwrap();
    assert!(!path.exists());
    assert_eq!(run(&repo, &["worktree", "list"]).lines().count(), 1);
    assert!(branch_exists(&repo, "herder/s1"));
}

#[tokio::test]
async fn reopen_keeps_a_worktree_still_there_and_replaces_a_broken_one() {
    let (_tmp, repo, worktrees) = setup();
    let worktree = worktrees.create(&repo, "s1", None).await.unwrap();
    let path = &worktree.path;
    std::fs::write(path.join("notes.txt"), "draft").unwrap();
    worktrees.reopen(&repo, path, "herder/s1").await.unwrap();
    assert_eq!(
        std::fs::read_to_string(path.join("notes.txt")).unwrap(),
        "draft"
    );

    std::fs::remove_file(path.join(".git")).unwrap();
    worktrees.reopen(&repo, path, "herder/s1").await.unwrap();
    assert!(!path.join("notes.txt").exists());
    assert_eq!(run(path, &["branch", "--show-current"]), "herder/s1");
}

#[tokio::test]
async fn remove_takes_a_clean_worktree_and_tolerates_a_missing_one() {
    let (_tmp, repo, worktrees) = setup();
    let worktree = worktrees.create(&repo, "s1", None).await.unwrap();
    std::fs::write(worktree.path.join("build.log"), "ignored").unwrap();
    std::fs::write(worktree.path.join(".gitignore"), "build.log\n").unwrap();
    run(&worktree.path, &["add", ".gitignore"]);
    run(&worktree.path, &["commit", "--quiet", "-m", "ignore logs"]);
    worktrees.remove(&repo, &worktree.path).await.unwrap();
    assert!(!worktree.path.exists());
    assert!(branch_exists(&repo, "herder/s1"));

    let other = worktrees.create(&repo, "s2", None).await.unwrap();
    std::fs::remove_dir_all(&other.path).unwrap();
    worktrees.remove(&repo, &other.path).await.unwrap();
    assert_eq!(run(&repo, &["worktree", "list"]).lines().count(), 1);
    assert_eq!(
        branches(&other.path, other.branch.as_deref())
            .await
            .unwrap(),
        ["herder/s2"]
    );
}

#[tokio::test]
async fn remove_leaves_paths_outside_the_worktrees_dir_alone() {
    let (_tmp, repo, worktrees) = setup();
    worktrees.remove(&repo, &repo).await.unwrap();
    assert!(repo.join(".git").is_dir());
}
