//! Project identity: daemons and clients must derive the same id from the same remote.

use herder_protocol::{HostId, ProjectId};

#[test]
fn remote_urls_normalise_to_host_and_path() {
    let cases = [
        ("git@github.com:org/repo.git", "github.com/org/repo"),
        ("https://github.com/org/repo", "github.com/org/repo"),
        ("https://github.com/org/repo.git", "github.com/org/repo"),
        ("https://github.com/org/repo/", "github.com/org/repo"),
        ("https://github.com/org/repo.git/", "github.com/org/repo"),
        ("http://github.com/org/repo", "github.com/org/repo"),
        ("ssh://git@github.com/org/repo.git", "github.com/org/repo"),
        (
            "ssh://git@github.com:22/org/repo.git",
            "github.com/org/repo",
        ),
        ("git+ssh://git@github.com/org/repo", "github.com/org/repo"),
        ("git://github.com/org/repo.git", "github.com/org/repo"),
        (
            "https://user:token@github.com/org/repo",
            "github.com/org/repo",
        ),
        ("https://GitHub.com/Org/Repo", "github.com/Org/Repo"),
        ("  git@github.com:org/repo.git\n", "github.com/org/repo"),
        ("git@github.com:/org//repo.git", "github.com/org/repo"),
        ("github.com:org/repo", "github.com/org/repo"),
        (
            "ssh://git@gitlab.example.com:2222/group/sub/repo.git",
            "gitlab.example.com/group/sub/repo",
        ),
        (
            "https://gitlab.example.com/group/sub/repo",
            "gitlab.example.com/group/sub/repo",
        ),
        ("ssh://git@[::1]:22/org/repo", "[::1]/org/repo"),
    ];
    for (url, id) in cases {
        assert_eq!(
            ProjectId::from_remote(url),
            Some(ProjectId::new(id)),
            "{url}"
        );
    }
}

#[test]
fn urls_without_a_remote_host_have_no_remote_id() {
    for url in [
        "",
        "/home/dev/repo",
        "./repo",
        "../repo.git",
        "file:///home/dev/repo.git",
        "FILE:///home/dev/repo",
        "https://github.com",
        "https://github.com/",
        "git@github.com:",
        "ssh://git@/org/repo",
        "./dir:with/colon",
    ] {
        assert_eq!(ProjectId::from_remote(url), None, "{url:?}");
    }
}

#[test]
fn local_projects_are_identified_by_host_and_path() {
    let id = ProjectId::local(&HostId::new("01J9HOST"), "/home/dev/scratch");
    assert_eq!(id.as_str(), "01J9HOST:/home/dev/scratch");
}
